use crate::types::{address::Address, core::NString};

/// The fields of the envelope structure are in the following
/// order: date, subject, from, sender, reply-to, to, cc, bcc,
/// in-reply-to, and message-id.
/// The date, subject, in-reply-to, and message-id fields are strings.
/// The from, sender, reply-to, to, cc, and bcc fields are parenthesized lists of address structures.
///
/// See [Address](struct.Address.html).
///
/// [RFC-2822] group syntax is indicated by a special form of
/// address structure in which the host name field is NIL.  If the
/// mailbox name field is also NIL, this is an end of group marker
/// (semi-colon in RFC 822 syntax).  If the mailbox name field is
/// non-NIL, this is a start of group marker, and the mailbox name
/// field holds the group name phrase.
///
/// If the Date, Subject, In-Reply-To, and Message-ID header lines
/// are absent in the [RFC-2822] header, the corresponding member
/// of the envelope is NIL; if these header lines are present but
/// empty the corresponding member of the envelope is the empty
/// string.
///
///    Note: some servers may return a NIL envelope member in the
///    "present but empty" case.  Clients SHOULD treat NIL and
///    empty string as identical.
///
///    Note: [RFC-2822] requires that all messages have a valid
///    Date header.  Therefore, the date member in the envelope can
///    not be NIL or the empty string.
///
///    Note: [RFC-2822] requires that the In-Reply-To and
///    Message-ID headers, if present, have non-empty content.
///    Therefore, the in-reply-to and message-id members in the
///    envelope can not be the empty string.
///
/// If the From, To, cc, and bcc header lines are absent in the
/// [RFC-2822] header, or are present but empty, the corresponding
/// member of the envelope is NIL.
///
/// If the Sender or Reply-To lines are absent in the [RFC-2822]
/// header, or are present but empty, the server sets the
/// corresponding member of the envelope to be the same value as
/// the from member (the client is not expected to know to do
/// this).
///
///    Note: [RFC-2822] requires that all messages have a valid
///    From header.  Therefore, the from, sender, and reply-to
///    members in the envelope can not be NIL.
/// TODO: many invariants here...
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub date: NString, // TODO: must not be empty string
    pub subject: NString,
    pub from: Vec<Address>,     // serialize as nil if empty?
    pub sender: Vec<Address>,   // TODO: set to from if absent or empty
    pub reply_to: Vec<Address>, // TODO: set to from if absent or empty
    pub to: Vec<Address>,       // serialize as nil if empty?
    pub cc: Vec<Address>,       // serialize as nil if empty?
    pub bcc: Vec<Address>,      // serialize as nil if empty?
    pub in_reply_to: NString,   // TODO: must not be empty string
    pub message_id: NString,    // TODO: must not be empty string
}

// FIXME
// impl std::fmt::Display for Envelope {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
//         write!(
//             f,
//             "({} {} {} ({}) ({}) {} {} {} {} {})",
//             self.date,
//             self.subject,
//             join_or_nil(&self.from, " "),
//             join(&self.sender, " "),   // FIXME: set to from if empty
//             join(&self.reply_to, " "), // FIXME: set to from if empty
//             join_or_nil(&self.to, " "),
//             join_or_nil(&self.cc, " "),
//             join_or_nil(&self.bcc, " "),
//             self.in_reply_to,
//             self.message_id,
//         )
//     }
// }