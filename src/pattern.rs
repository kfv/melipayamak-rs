// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2026 Faraz Vahedi <kfv@FreeBSD.org>

//! Managing the account's patterns (الگوی خدماتی), the approved texts that
//! [`Client::send_pattern`] fills in (`/api/ShareService/…`).

use serde::{Deserialize, Serialize};

use crate::{Client, Error, value};

int_enum! {
    /// Review status of a pattern.
    PatternStatus {
        /// `0`: waiting for approval.
        Wait = 0,
        /// `1`: approved.
        Approved = 1,
        /// `2`: denied.
        Denied = 2,
        /// `3`: edited, waiting for approval.
        EditedAndWait = 3,
        /// `4`: needs editing; only now can it be edited.
        NeedToEdit = 4,
        /// `5`: deleted.  Never listed.
        Deleted = 5,
        /// `6`: denied by an administrator.
        DeniedByAdmin = 6,
    }
}

/// Request for [`Client::patterns`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PatternsRequest {
    /// Matched against title, text and ID.  The ID match is not exact, so
    /// check [`Pattern::body_id`] in the results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// Only patterns in this status; all listable ones if `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<PatternStatus>,
}

/// A pattern, as listed by [`Client::patterns`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[non_exhaustive]
pub struct Pattern {
    /// Pattern ID, used as `body_id` when sending.
    #[serde(rename = "BodyID")]
    pub body_id: i32,
    /// Title.
    pub title: String,
    /// Text with its `{0}`…`{19}` placeholders.
    pub body: String,
    /// Review status.
    pub body_status: PatternStatus,
    /// Reviewer's note, if any.
    pub description: Option<String>,
    /// When it was added (ISO 8601).
    pub insert_date: String,
}

/// Request for [`Client::add_pattern`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddPattern {
    /// Title, at most 50 characters.
    pub title: String,
    /// Text with at least one placeholder from `{0}` to `{19}`.
    /// Melipayamak strips the standard opt-out phrases from it.
    pub body: String,
}

/// Request for [`Client::edit_pattern`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditPattern {
    /// The pattern to change.
    #[serde(rename = "bodyId")]
    pub body_id: i32,
    /// New title, at most 50 characters.
    pub title: String,
    /// New text with at least one placeholder from `{0}` to `{19}`.  Unlike
    /// [`Client::add_pattern`], opt-out phrases are not stripped.
    pub body: String,
}

#[derive(Serialize)]
struct BodyIdBody {
    #[serde(rename = "bodyId")]
    body_id: i32,
}

impl Client {
    /// Lists the account's patterns, newest first.
    ///
    /// `POST /api/ShareService/GetBodies`, JSON body
    /// (<https://www.melipayamak.com/api/getbodies/>).
    pub async fn patterns(
        &self,
        req: &PatternsRequest,
    ) -> Result<Vec<Pattern>, Error> {
        let r = self.post_json("api/ShareService/GetBodies", req).await?;
        value::one(&r)?;
        value::records(&r)
    }

    /// Adds a pattern, which then waits for approval.  Returns its ID.
    ///
    /// Melipayamak does not detect duplicates; after an uncertain outcome,
    /// list the patterns before adding again.
    ///
    /// `POST /api/ShareService/AddBody`, JSON body
    /// (<https://www.melipayamak.com/api/addbody/>).
    pub async fn add_pattern(&self, req: &AddPattern) -> Result<i32, Error> {
        let r = self.post_json("api/ShareService/AddBody", req).await?;
        let id = value::positive_id(&r)?;
        i32::try_from(id).map_err(|_| r.unexpected())
    }

    /// Replaces a pattern's title and text; it then waits for approval
    /// again.  Only a pattern in [`PatternStatus::NeedToEdit`] can be
    /// edited.
    ///
    /// `POST /api/ShareService/EditBody`, JSON body
    /// (<https://www.melipayamak.com/api/editbody/>).
    pub async fn edit_pattern(&self, req: &EditPattern) -> Result<(), Error> {
        let r = self.post_json("api/ShareService/EditBody", req).await?;
        value::one(&r)
    }

    /// Deletes a pattern.  Deleting one already deleted succeeds.
    ///
    /// `POST /api/ShareService/DeleteBody`, JSON body
    /// (<https://www.melipayamak.com/api/deletebody/>).
    pub async fn delete_pattern(&self, body_id: i32) -> Result<(), Error> {
        let body = BodyIdBody { body_id };
        let r = self.post_json("api/ShareService/DeleteBody", &body).await?;
        value::one(&r)
    }
}
