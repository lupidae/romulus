//! Approvals and review requests in a room (docs/MVP.md "Review round 1").
//!
//! The room knows who edited the SQL (it sees every update: the first one
//! of a person's that changes the text makes them an editor, recorded in
//! `draft_editors`), and the approvals given, each of one text: an approval
//! counts while the SQL hashes as it did when it was given and its author
//! has not edited the draft, so any edit resets them (and taking the edit
//! back brings them back: it is the text they approved). Everyone hears of
//! a change as a [`ServerEvent::Review`]; publishing asks the room who
//! approved the frozen text ([`super::room::Content::approved_by`]), which
//! nothing written later can contradict.
//!
//! `drafts::approvals` writes the approvals and review requests and tells
//! the room; the room writes the editors.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::protocol::{ReviewApproval, ReviewRequest, ServerEvent};

/// One member's approval, of the text whose SHA-256 is `sha256` (hex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub user_id: Uuid,
    pub name: String,
    pub sha256: String,
    pub at: DateTime<Utc>,
}

/// Why an approval is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalError {
    /// Being published or updated, or no longer open.
    #[error("the draft is not open")]
    NotOpen,
    /// The text is not the one the person approved: they review it again.
    #[error("the draft changed")]
    Changed,
    /// They edited the SQL: someone else approves it.
    #[error("an editor of the draft cannot approve it")]
    Editor,
}

/// What a room keeps of its draft's review.
#[derive(Debug, Clone, Default)]
pub struct Review {
    /// The organization's setting: approvals publishing needs (0: none).
    pub required: i32,
    pub editors: HashSet<Uuid>,
    pub approvals: Vec<Approval>,
    pub requested: Option<ReviewRequest>,
}

impl Review {
    /// Who approved the text hashing to `sha256` without having edited the
    /// draft: the approvals that count, oldest first.
    pub fn approved_by(&self, sha256: &str) -> Vec<Uuid> {
        self.counted(sha256).map(|a| a.user_id).collect()
    }

    fn counted<'a>(&'a self, sha256: &'a str) -> impl Iterator<Item = &'a Approval> {
        self.approvals
            .iter()
            .filter(move |a| a.sha256 == sha256 && !self.editors.contains(&a.user_id))
    }

    /// Adds or replaces `approval`'s author's approval.
    pub fn approve(&mut self, approval: Approval) {
        self.approvals.retain(|a| a.user_id != approval.user_id);
        self.approvals.push(approval);
        self.approvals.sort_by_key(|a| a.at);
    }

    pub fn withdraw(&mut self, user_id: Uuid) {
        self.approvals.retain(|a| a.user_id != user_id);
    }

    /// The event everyone gets, for the text hashing to `sha256`.
    pub fn event(&self, sha256: &str) -> ServerEvent {
        let view = |a: &Approval| ReviewApproval {
            user_id: a.user_id,
            name: a.name.clone(),
            at: a.at,
        };
        let mut editors: Vec<Uuid> = self.editors.iter().copied().collect();
        editors.sort_unstable();
        ServerEvent::Review {
            required: self.required,
            approvals: self.counted(sha256).map(view).collect(),
            outdated: self
                .approvals
                .iter()
                .filter(|a| a.sha256 != sha256 && !self.editors.contains(&a.user_id))
                .map(view)
                .collect(),
            editors,
            requested: self.requested.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(user: u128, sha256: &str) -> Approval {
        Approval {
            user_id: Uuid::from_u128(user),
            name: format!("user {user}"),
            sha256: sha256.into(),
            at: DateTime::from_timestamp(i64::try_from(user).unwrap(), 0).unwrap(),
        }
    }

    #[test]
    fn approvals_count_for_their_text_only() {
        let mut review = Review::default();
        review.approve(approval(1, "a"));
        review.approve(approval(2, "b"));
        assert_eq!(review.approved_by("a"), [Uuid::from_u128(1)]);
        assert_eq!(review.approved_by("b"), [Uuid::from_u128(2)]);
        let ServerEvent::Review {
            approvals,
            outdated,
            ..
        } = review.event("a")
        else {
            unreachable!()
        };
        assert_eq!(approvals.len(), 1);
        assert_eq!(outdated[0].user_id, Uuid::from_u128(2));
    }

    #[test]
    fn an_editors_approval_never_counts() {
        let mut review = Review::default();
        review.approve(approval(1, "a"));
        review.editors.insert(Uuid::from_u128(1));
        assert_eq!(review.approved_by("a"), Vec::<Uuid>::new());
        let ServerEvent::Review { outdated, .. } = review.event("b") else {
            unreachable!()
        };
        assert!(outdated.is_empty(), "nor shows as outdated");
    }

    #[test]
    fn approving_again_replaces_the_approval() {
        let mut review = Review::default();
        review.approve(approval(1, "a"));
        review.approve(approval(1, "b"));
        assert_eq!(review.approvals.len(), 1);
        assert_eq!(review.approved_by("b"), [Uuid::from_u128(1)]);
        review.withdraw(Uuid::from_u128(1));
        assert_eq!(review.approvals, []);
    }
}
