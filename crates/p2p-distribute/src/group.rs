// SPDX-License-Identifier: MIT OR Apache-2.0

//! Closed share group membership — role-based access control for
//! managed replication groups.
//!
//! ## What
//!
//! A *group* is a set of peers that cooperate to replicate a signed
//! content catalog. The group has a single **master** who publishes
//! catalog updates; **admins** who can manage membership; and
//! **mirrors** who replicate content read-only.
//!
//! ## Why
//!
//! Self-hosted content shops: anyone can stand up a group, populate it
//! with files, and let mirror nodes increase availability without
//! trusting them with write access. Mirrors verify catalog updates
//! cryptographically (via [`GroupManifest`] signatures), without
//! trusting the transport.
//!
//! ## How
//!
//! - [`GroupRole`] — Master / Admin / Mirror / Reader hierarchy.
//! - [`GroupMember`] — a peer's identity + role + join timestamp.
//! - [`GroupRoster`] — the group's membership list with role-based
//!   mutation rules (only master/admin can add members, only master
//!   can promote to admin, etc.).
//!
//! The roster feeds into [`catalog::plan_sync`] and [`manifest::diff_manifests`]
//! to determine what each mirror should replicate.
//!
//! [`GroupManifest`]: crate::manifest::GroupManifest
//! [`catalog::plan_sync`]: crate::catalog::plan_sync
//! [`manifest::diff_manifests`]: crate::manifest::diff_manifests

use std::collections::HashMap;

use crate::network_id::NetworkId;
use crate::peer_id::PeerId;

use thiserror::Error;

// ── Constants ───────────────────────────────────────────────────────

/// Maximum number of members in a single group.
///
/// Prevents unbounded growth. Mirrors beyond this limit should form a
/// second group or use DHT-based discovery instead. 500 is generous for
/// a managed replication group — most will have 2–20 members.
pub const MAX_GROUP_MEMBERS: usize = 500;

// ── GroupRole ───────────────────────────────────────────────────────

/// Role within a closed share group, ordered by privilege level.
///
/// The hierarchy is: Master > Admin > Mirror > Reader.
///
/// - **Master** — publishes catalog updates, manages all membership.
///   Exactly one per group.
/// - **Admin** — can add/remove mirrors and readers, but cannot publish
///   catalogs or change the master.
/// - **Mirror** — replicates content per the replication policy. Read-only
///   access to the catalog. Can serve pieces to other peers.
/// - **Reader** — can download content but does not replicate or serve
///   pieces. Useful for end-user clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupRole {
    Master,
    Admin,
    Mirror,
    Reader,
}

impl GroupRole {
    /// Returns the numeric privilege level (higher = more privileged).
    ///
    /// Used internally for permission checks: an actor can only manage
    /// members with a strictly lower privilege level than their own.
    fn privilege_level(self) -> u8 {
        match self {
            Self::Master => 3,
            Self::Admin => 2,
            Self::Mirror => 1,
            Self::Reader => 0,
        }
    }

    /// Whether this role can manage (add/remove/demote) a member with
    /// the given target role.
    ///
    /// Rule: actor privilege must be strictly greater than target privilege.
    /// Master can manage anyone. Admin can manage Mirror and Reader.
    /// Mirror and Reader cannot manage anyone.
    pub fn can_manage(self, target: GroupRole) -> bool {
        // Only Master and Admin may manage; Mirror and Reader have no
        // management authority regardless of relative privilege.
        match self {
            Self::Master | Self::Admin => self.privilege_level() > target.privilege_level(),
            Self::Mirror | Self::Reader => false,
        }
    }

    /// Whether this role can publish catalog updates.
    ///
    /// Only the Master can publish. Admins can manage membership but
    /// cannot change the catalog — separation of concerns.
    pub fn can_publish(self) -> bool {
        matches!(self, Self::Master)
    }

    /// Whether this role can serve content pieces to other peers.
    ///
    /// Master, Admin, and Mirror can serve; Readers are download-only.
    pub fn can_serve(self) -> bool {
        !matches!(self, Self::Reader)
    }
}

impl std::fmt::Display for GroupRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Master => write!(f, "master"),
            Self::Admin => write!(f, "admin"),
            Self::Mirror => write!(f, "mirror"),
            Self::Reader => write!(f, "reader"),
        }
    }
}

// ── GroupMember ─────────────────────────────────────────────────────

/// A member of a closed share group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    /// The member's peer identity.
    pub peer_id: PeerId,
    /// The member's role in the group.
    pub role: GroupRole,
    /// Monotonic join sequence number (assigned by the roster).
    ///
    /// Used for deterministic ordering when multiple members have the
    /// same role. Lower = joined earlier.
    pub join_seq: u64,
}

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from group roster operations.
#[derive(Debug, Error)]
pub enum GroupError {
    #[error("group is full ({max} members)")]
    GroupFull { max: usize },
    #[error("peer {peer_id} is already a member with role {existing_role}")]
    AlreadyMember {
        peer_id: PeerId,
        existing_role: GroupRole,
    },
    #[error("peer {peer_id} is not a member of this group")]
    NotMember { peer_id: PeerId },
    #[error("{actor_role} cannot manage {target_role} members (insufficient privilege)")]
    InsufficientPrivilege {
        actor_role: GroupRole,
        target_role: GroupRole,
    },
    #[error("cannot remove the group master — transfer ownership first")]
    CannotRemoveMaster,
    #[error("cannot demote the group master — transfer ownership first")]
    CannotDemoteMaster,
    #[error("only the master can promote members to admin")]
    OnlyMasterCanPromoteAdmin,
    #[error("group must have exactly one master")]
    MultipleMasters,
}

// ── GroupRoster ─────────────────────────────────────────────────────

/// Manages membership for a closed share group.
///
/// Enforces the role-based access control invariants:
/// - Exactly one Master at all times.
/// - Admins can add/remove Mirrors and Readers.
/// - Only Master can promote to Admin or transfer ownership.
/// - Members are uniquely identified by [`PeerId`].
pub struct GroupRoster {
    /// The network this group belongs to (for PEX/DHT isolation).
    network_id: NetworkId,
    /// Members keyed by PeerId for O(1) lookup.
    members: HashMap<PeerId, GroupMember>,
    /// Monotonic counter for join sequence assignment.
    next_seq: u64,
}

impl GroupRoster {
    /// Creates a new group with the given master.
    ///
    /// Every group must have exactly one master from creation. The master
    /// is the first member (join_seq = 0).
    pub fn new(network_id: NetworkId, master_id: PeerId) -> Self {
        let mut members = HashMap::new();
        members.insert(
            master_id,
            GroupMember {
                peer_id: master_id,
                role: GroupRole::Master,
                join_seq: 0,
            },
        );
        Self {
            network_id,
            members,
            next_seq: 1,
        }
    }

    /// Returns the network ID for this group.
    pub fn network_id(&self) -> NetworkId {
        self.network_id
    }

    /// Returns the number of members in the group.
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    /// Returns the master's PeerId.
    pub fn master_id(&self) -> PeerId {
        // Invariant: exactly one master always exists.
        self.members
            .values()
            .find(|m| m.role == GroupRole::Master)
            .map(|m| m.peer_id)
            .expect("group invariant violated: no master")
    }

    /// Looks up a member by PeerId.
    pub fn get(&self, peer_id: &PeerId) -> Option<&GroupMember> {
        self.members.get(peer_id)
    }

    /// Returns the role of a peer, or `None` if not a member.
    pub fn role_of(&self, peer_id: &PeerId) -> Option<GroupRole> {
        self.members.get(peer_id).map(|m| m.role)
    }

    /// Adds a new member to the group.
    ///
    /// The `actor` must have sufficient privilege to add a member with
    /// the given `role`. Returns error if the group is full, the peer
    /// is already a member, or the actor lacks privilege.
    pub fn add_member(
        &mut self,
        actor: &PeerId,
        new_peer: PeerId,
        role: GroupRole,
    ) -> Result<&GroupMember, GroupError> {
        // Cannot add a second master.
        if role == GroupRole::Master {
            return Err(GroupError::MultipleMasters);
        }

        // Only master can add admins.
        if role == GroupRole::Admin {
            let actor_role = self.require_member_role(actor)?;
            if actor_role != GroupRole::Master {
                return Err(GroupError::OnlyMasterCanPromoteAdmin);
            }
        } else {
            let actor_role = self.require_member_role(actor)?;
            if !actor_role.can_manage(role) {
                return Err(GroupError::InsufficientPrivilege {
                    actor_role,
                    target_role: role,
                });
            }
        }

        if self.members.contains_key(&new_peer) {
            let existing = self.members.get(&new_peer).expect("just checked");
            return Err(GroupError::AlreadyMember {
                peer_id: new_peer,
                existing_role: existing.role,
            });
        }

        if self.members.len() >= MAX_GROUP_MEMBERS {
            return Err(GroupError::GroupFull {
                max: MAX_GROUP_MEMBERS,
            });
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);

        let member = GroupMember {
            peer_id: new_peer,
            role,
            join_seq: seq,
        };
        self.members.insert(new_peer, member);
        Ok(self.members.get(&new_peer).expect("just inserted"))
    }

    /// Removes a member from the group.
    ///
    /// The `actor` must have sufficient privilege. The master cannot be
    /// removed — use `transfer_master` first.
    pub fn remove_member(
        &mut self,
        actor: &PeerId,
        target: &PeerId,
    ) -> Result<GroupMember, GroupError> {
        let target_role = self.require_member_role(target)?;

        if target_role == GroupRole::Master {
            return Err(GroupError::CannotRemoveMaster);
        }

        let actor_role = self.require_member_role(actor)?;
        if !actor_role.can_manage(target_role) {
            return Err(GroupError::InsufficientPrivilege {
                actor_role,
                target_role,
            });
        }

        self.members
            .remove(target)
            .ok_or(GroupError::NotMember { peer_id: *target })
    }

    /// Changes a member's role (promote or demote).
    ///
    /// Only the master can promote to admin. Admins can demote mirrors
    /// to readers and vice versa. Cannot change the master's role.
    pub fn change_role(
        &mut self,
        actor: &PeerId,
        target: &PeerId,
        new_role: GroupRole,
    ) -> Result<(), GroupError> {
        let target_member = self
            .members
            .get(target)
            .ok_or(GroupError::NotMember { peer_id: *target })?;

        if target_member.role == GroupRole::Master {
            return Err(GroupError::CannotDemoteMaster);
        }

        if new_role == GroupRole::Master {
            return Err(GroupError::MultipleMasters);
        }

        if new_role == GroupRole::Admin {
            let actor_role = self.require_member_role(actor)?;
            if actor_role != GroupRole::Master {
                return Err(GroupError::OnlyMasterCanPromoteAdmin);
            }
        } else {
            let actor_role = self.require_member_role(actor)?;
            if !actor_role.can_manage(target_member.role) && !actor_role.can_manage(new_role) {
                return Err(GroupError::InsufficientPrivilege {
                    actor_role,
                    target_role: target_member.role,
                });
            }
        }

        if let Some(member) = self.members.get_mut(target) {
            member.role = new_role;
        }

        Ok(())
    }

    /// Transfers master ownership to another member.
    ///
    /// The current master becomes an Admin. The target becomes Master.
    /// Only the current master can initiate this.
    pub fn transfer_master(
        &mut self,
        current_master: &PeerId,
        new_master: &PeerId,
    ) -> Result<(), GroupError> {
        let actor_role = self.require_member_role(current_master)?;
        if actor_role != GroupRole::Master {
            return Err(GroupError::InsufficientPrivilege {
                actor_role,
                target_role: GroupRole::Master,
            });
        }

        // Target must be a member.
        let _target_role = self.require_member_role(new_master)?;

        // Demote current master to admin.
        if let Some(m) = self.members.get_mut(current_master) {
            m.role = GroupRole::Admin;
        }

        // Promote target to master.
        if let Some(m) = self.members.get_mut(new_master) {
            m.role = GroupRole::Master;
        }

        Ok(())
    }

    /// Lists all members with the given role.
    pub fn members_with_role(&self, role: GroupRole) -> Vec<&GroupMember> {
        let mut result: Vec<_> = self.members.values().filter(|m| m.role == role).collect();
        result.sort_by_key(|m| m.join_seq);
        result
    }

    /// Lists all members, sorted by join sequence.
    pub fn all_members(&self) -> Vec<&GroupMember> {
        let mut result: Vec<_> = self.members.values().collect();
        result.sort_by_key(|m| m.join_seq);
        result
    }

    /// Returns the role of an actor, or `NotMember` error.
    fn require_member_role(&self, peer_id: &PeerId) -> Result<GroupRole, GroupError> {
        self.members
            .get(peer_id)
            .map(|m| m.role)
            .ok_or(GroupError::NotMember { peer_id: *peer_id })
    }
}

impl std::fmt::Debug for GroupRoster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupRoster")
            .field("network_id", &self.network_id)
            .field("member_count", &self.members.len())
            .field("next_seq", &self.next_seq)
            .finish()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn master_id() -> PeerId {
        PeerId::from_key_material(b"master")
    }
    fn admin_id() -> PeerId {
        PeerId::from_key_material(b"admin")
    }
    fn mirror_id() -> PeerId {
        PeerId::from_key_material(b"mirror")
    }
    fn reader_id() -> PeerId {
        PeerId::from_key_material(b"reader")
    }
    fn peer_n(n: u32) -> PeerId {
        PeerId::from_key_material(&n.to_le_bytes())
    }

    // ── GroupRole ───────────────────────────────────────────────────

    /// Master can manage all other roles.
    #[test]
    fn master_can_manage_all() {
        assert!(GroupRole::Master.can_manage(GroupRole::Admin));
        assert!(GroupRole::Master.can_manage(GroupRole::Mirror));
        assert!(GroupRole::Master.can_manage(GroupRole::Reader));
        assert!(!GroupRole::Master.can_manage(GroupRole::Master));
    }

    /// Admin can manage Mirror and Reader, not Master or Admin.
    #[test]
    fn admin_can_manage_lower() {
        assert!(GroupRole::Admin.can_manage(GroupRole::Mirror));
        assert!(GroupRole::Admin.can_manage(GroupRole::Reader));
        assert!(!GroupRole::Admin.can_manage(GroupRole::Admin));
        assert!(!GroupRole::Admin.can_manage(GroupRole::Master));
    }

    /// Mirror and Reader cannot manage anyone.
    #[test]
    fn mirror_reader_cannot_manage() {
        assert!(!GroupRole::Mirror.can_manage(GroupRole::Reader));
        assert!(!GroupRole::Reader.can_manage(GroupRole::Reader));
    }

    /// Only Master can publish catalog updates.
    #[test]
    fn only_master_can_publish() {
        assert!(GroupRole::Master.can_publish());
        assert!(!GroupRole::Admin.can_publish());
        assert!(!GroupRole::Mirror.can_publish());
        assert!(!GroupRole::Reader.can_publish());
    }

    /// Reader cannot serve content; all others can.
    #[test]
    fn reader_cannot_serve() {
        assert!(GroupRole::Master.can_serve());
        assert!(GroupRole::Admin.can_serve());
        assert!(GroupRole::Mirror.can_serve());
        assert!(!GroupRole::Reader.can_serve());
    }

    /// Display format for all roles.
    #[test]
    fn role_display() {
        assert_eq!(GroupRole::Master.to_string(), "master");
        assert_eq!(GroupRole::Admin.to_string(), "admin");
        assert_eq!(GroupRole::Mirror.to_string(), "mirror");
        assert_eq!(GroupRole::Reader.to_string(), "reader");
    }

    // ── GroupRoster creation ────────────────────────────────────────

    /// New roster has exactly one member (the master).
    #[test]
    fn new_roster_has_master() {
        let roster = GroupRoster::new(NetworkId::TEST, master_id());
        assert_eq!(roster.member_count(), 1);
        assert_eq!(roster.master_id(), master_id());
        assert_eq!(roster.role_of(&master_id()), Some(GroupRole::Master));
    }

    /// Network ID is preserved.
    #[test]
    fn roster_network_id() {
        let roster = GroupRoster::new(NetworkId::PRODUCTION, master_id());
        assert_eq!(roster.network_id(), NetworkId::PRODUCTION);
    }

    // ── Adding members ──────────────────────────────────────────────

    /// Master can add a mirror.
    #[test]
    fn master_adds_mirror() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        let member = roster
            .add_member(&master_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();
        assert_eq!(member.role, GroupRole::Mirror);
        assert_eq!(roster.member_count(), 2);
    }

    /// Master can add an admin.
    #[test]
    fn master_adds_admin() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), admin_id(), GroupRole::Admin)
            .unwrap();
        assert_eq!(roster.role_of(&admin_id()), Some(GroupRole::Admin));
    }

    /// Admin can add mirrors and readers.
    #[test]
    fn admin_adds_mirror_and_reader() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), admin_id(), GroupRole::Admin)
            .unwrap();

        roster
            .add_member(&admin_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();
        roster
            .add_member(&admin_id(), reader_id(), GroupRole::Reader)
            .unwrap();
        assert_eq!(roster.member_count(), 4);
    }

    /// Admin cannot add another admin.
    #[test]
    fn admin_cannot_add_admin() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), admin_id(), GroupRole::Admin)
            .unwrap();

        let peer2 = PeerId::from_key_material(b"admin2");
        let err = roster
            .add_member(&admin_id(), peer2, GroupRole::Admin)
            .unwrap_err();
        assert!(matches!(err, GroupError::OnlyMasterCanPromoteAdmin));
    }

    /// Mirror cannot add anyone.
    #[test]
    fn mirror_cannot_add() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();

        let err = roster
            .add_member(&mirror_id(), reader_id(), GroupRole::Reader)
            .unwrap_err();
        assert!(matches!(err, GroupError::InsufficientPrivilege { .. }));
    }

    /// Cannot add a second master.
    #[test]
    fn cannot_add_second_master() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        let err = roster
            .add_member(&master_id(), mirror_id(), GroupRole::Master)
            .unwrap_err();
        assert!(matches!(err, GroupError::MultipleMasters));
    }

    /// Duplicate member is rejected.
    #[test]
    fn duplicate_member_rejected() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();
        let err = roster
            .add_member(&master_id(), mirror_id(), GroupRole::Mirror)
            .unwrap_err();
        assert!(matches!(err, GroupError::AlreadyMember { .. }));
    }

    /// Non-member actor is rejected.
    #[test]
    fn non_member_actor_rejected() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        let stranger = PeerId::from_key_material(b"stranger");
        let err = roster
            .add_member(&stranger, mirror_id(), GroupRole::Mirror)
            .unwrap_err();
        assert!(matches!(err, GroupError::NotMember { .. }));
    }

    // ── Removing members ────────────────────────────────────────────

    /// Master can remove a mirror.
    #[test]
    fn master_removes_mirror() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();
        let removed = roster.remove_member(&master_id(), &mirror_id()).unwrap();
        assert_eq!(removed.role, GroupRole::Mirror);
        assert_eq!(roster.member_count(), 1);
    }

    /// Cannot remove the master.
    #[test]
    fn cannot_remove_master() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        let err = roster
            .remove_member(&master_id(), &master_id())
            .unwrap_err();
        assert!(matches!(err, GroupError::CannotRemoveMaster));
    }

    /// Admin can remove mirrors and readers.
    #[test]
    fn admin_removes_mirror() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), admin_id(), GroupRole::Admin)
            .unwrap();
        roster
            .add_member(&admin_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();
        roster.remove_member(&admin_id(), &mirror_id()).unwrap();
        assert_eq!(roster.member_count(), 2);
    }

    // ── Role changes ────────────────────────────────────────────────

    /// Master promotes mirror to admin.
    #[test]
    fn master_promotes_to_admin() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();
        roster
            .change_role(&master_id(), &mirror_id(), GroupRole::Admin)
            .unwrap();
        assert_eq!(roster.role_of(&mirror_id()), Some(GroupRole::Admin));
    }

    /// Admin cannot promote to admin.
    #[test]
    fn admin_cannot_promote_to_admin() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), admin_id(), GroupRole::Admin)
            .unwrap();
        roster
            .add_member(&admin_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();
        let err = roster
            .change_role(&admin_id(), &mirror_id(), GroupRole::Admin)
            .unwrap_err();
        assert!(matches!(err, GroupError::OnlyMasterCanPromoteAdmin));
    }

    /// Cannot demote the master.
    #[test]
    fn cannot_demote_master() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        let err = roster
            .change_role(&master_id(), &master_id(), GroupRole::Admin)
            .unwrap_err();
        assert!(matches!(err, GroupError::CannotDemoteMaster));
    }

    // ── Master transfer ─────────────────────────────────────────────

    /// Master can transfer ownership to another member.
    #[test]
    fn transfer_master() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), admin_id(), GroupRole::Admin)
            .unwrap();
        roster.transfer_master(&master_id(), &admin_id()).unwrap();
        assert_eq!(roster.role_of(&admin_id()), Some(GroupRole::Master));
        assert_eq!(roster.role_of(&master_id()), Some(GroupRole::Admin));
        assert_eq!(roster.master_id(), admin_id());
    }

    /// Non-master cannot transfer ownership.
    #[test]
    fn non_master_cannot_transfer() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), admin_id(), GroupRole::Admin)
            .unwrap();
        let err = roster
            .transfer_master(&admin_id(), &master_id())
            .unwrap_err();
        assert!(matches!(err, GroupError::InsufficientPrivilege { .. }));
    }

    // ── Queries ─────────────────────────────────────────────────────

    /// `members_with_role` returns sorted by join sequence.
    #[test]
    fn members_with_role_sorted() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        let m1 = PeerId::from_key_material(b"mirror1");
        let m2 = PeerId::from_key_material(b"mirror2");
        roster
            .add_member(&master_id(), m1, GroupRole::Mirror)
            .unwrap();
        roster
            .add_member(&master_id(), m2, GroupRole::Mirror)
            .unwrap();
        let mirrors = roster.members_with_role(GroupRole::Mirror);
        assert_eq!(mirrors.len(), 2);
        assert!(mirrors[0].join_seq < mirrors[1].join_seq);
    }

    /// `all_members` returns everyone sorted by join sequence.
    #[test]
    fn all_members_sorted() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        roster
            .add_member(&master_id(), admin_id(), GroupRole::Admin)
            .unwrap();
        roster
            .add_member(&master_id(), mirror_id(), GroupRole::Mirror)
            .unwrap();
        let all = roster.all_members();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].role, GroupRole::Master);
    }

    /// Looking up a non-member returns None.
    #[test]
    fn get_non_member_returns_none() {
        let roster = GroupRoster::new(NetworkId::TEST, master_id());
        assert!(roster.get(&mirror_id()).is_none());
        assert!(roster.role_of(&mirror_id()).is_none());
    }

    // ── Error display ───────────────────────────────────────────────

    /// GroupFull error includes the maximum.
    #[test]
    fn error_display_group_full() {
        let err = GroupError::GroupFull { max: 500 };
        assert!(err.to_string().contains("500"));
    }

    /// InsufficientPrivilege error includes both roles.
    #[test]
    fn error_display_insufficient_privilege() {
        let err = GroupError::InsufficientPrivilege {
            actor_role: GroupRole::Mirror,
            target_role: GroupRole::Admin,
        };
        let msg = err.to_string();
        assert!(msg.contains("mirror"), "should contain actor role: {msg}");
        assert!(msg.contains("admin"), "should contain target role: {msg}");
    }

    // ── Boundary: group full ────────────────────────────────────────

    /// Adding beyond MAX_GROUP_MEMBERS fails.
    #[test]
    fn group_full_boundary() {
        let mut roster = GroupRoster::new(NetworkId::TEST, master_id());
        // Fill to max (master is already member 0).
        for i in 1..MAX_GROUP_MEMBERS as u32 {
            roster
                .add_member(&master_id(), peer_n(i), GroupRole::Mirror)
                .unwrap();
        }
        assert_eq!(roster.member_count(), MAX_GROUP_MEMBERS);
        let err = roster
            .add_member(
                &master_id(),
                peer_n(MAX_GROUP_MEMBERS as u32),
                GroupRole::Mirror,
            )
            .unwrap_err();
        assert!(matches!(err, GroupError::GroupFull { .. }));
    }

    // ── Debug ───────────────────────────────────────────────────────

    /// Debug output includes member count.
    #[test]
    fn debug_includes_member_count() {
        let roster = GroupRoster::new(NetworkId::TEST, master_id());
        let debug = format!("{roster:?}");
        assert!(debug.contains("member_count"), "debug: {debug}");
    }
}
