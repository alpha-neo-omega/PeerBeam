//! Read-only browsing of folders a device chose to share.
//!
//! Two things gate every answer, and they are deliberately independent:
//! `Permission::Browse` (whom the user trusts to look) and the configured
//! shares (what there is to look at). **The default share list is empty**, so a
//! device that grants the permission and shares nothing still answers every
//! request with nothing.

mod handler;
mod message;
mod share;

pub use handler::{list, AnswerSink, BrowseHandler, IncomingSink};
pub use message::{
    BrowseError, Entry, ListRequest, ListResponse, MAX_ENTRIES, MAX_PATH, MSG_LIST_REQUEST,
    MSG_LIST_RESPONSE,
};
pub use share::{Share, ShareError, Shares};
