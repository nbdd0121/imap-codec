use std::{io::Write, num::NonZeroU32};

use base64::{engine::general_purpose::STANDARD as base64, Engine};
use chrono::{DateTime as ChronoDateTime, FixedOffset};
#[cfg(feature = "ext_literal")]
use imap_types::core::LiteralMode;
use imap_types::{
    auth::{AuthMechanism, AuthenticateData},
    body::{
        BasicFields, Body, BodyExtension, BodyStructure, Disposition, Language, Location,
        MultiPartExtensionData, SinglePartExtensionData, SpecificFields,
    },
    command::{Command, CommandBody},
    core::{
        AString, Atom, AtomExt, Charset, IString, Literal, NString, Quoted, QuotedChar, Tag, Text,
    },
    datetime::{DateTime, NaiveDate},
    envelope::{Address, Envelope},
    fetch::{
        Macro, MacroOrMessageDataItemNames, MessageDataItem, MessageDataItemName, Part, Section,
    },
    flag::{Flag, FlagExtension, FlagFetch, FlagNameAttribute, FlagPerm, StoreResponse, StoreType},
    mailbox::{ListCharString, ListMailbox, Mailbox, MailboxOther},
    response::{
        Capability, Code, CodeOther, Continue, Data, Greeting, GreetingKind, Response, Status,
    },
    search::SearchKey,
    sequence::{SeqOrUid, Sequence, SequenceSet},
    status::{StatusDataItem, StatusDataItemName},
    utils::escape_quoted,
};
use utils::{join_serializable, List1AttributeValueOrNil, List1OrNil};

pub trait Encode {
    /// Create an [`Encoded`] for this message.
    fn encode(&self) -> Encoded;
}

/// Message encoder.
///
/// This encoder facilitates the implementation of IMAP client- and server implementations by
/// yielding the encoding of a message through [`Fragment`]s. This is required, because the usage of
/// literals (and some other types) may change the IMAP message flow. Thus, in many cases, it is an
/// error to just "dump" a message and send it over the network.
///
/// # Example
///
/// ```rust
/// use imap_codec::{
///     codec::{Encode, Fragment},
///     imap_types::command::{Command, CommandBody},
/// };
///
/// let cmd = Command::new("A", CommandBody::login("alice", "pass").unwrap()).unwrap();
///
/// for fragment in cmd.encode() {
///     match fragment {
///         Fragment::Line { data } => {}
///         #[cfg(not(feature = "ext_literal"))]
///         Fragment::Literal { data } => {}
///         #[cfg(feature = "ext_literal")]
///         Fragment::Literal { data, mode } => {}
///     }
/// }
/// ```
#[derive(Clone, Debug)]
pub struct Encoded {
    items: Vec<Fragment>,
}

impl Encoded {
    /// Dump the (remaining) encoded data without being guided by [`Fragment`]s.
    pub fn dump(self) -> Vec<u8> {
        let mut out = Vec::new();

        for fragment in self.items {
            match fragment {
                Fragment::Line { mut data } => out.append(&mut data),
                Fragment::Literal { mut data, .. } => out.append(&mut data),
            }
        }

        out
    }
}

impl Iterator for Encoded {
    type Item = Fragment;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.items.is_empty() {
            Some(self.items.remove(0))
        } else {
            None
        }
    }
}

/// The intended action of a client or server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fragment {
    /// A line that is ready to be send.
    Line { data: Vec<u8> },

    /// A literal that may require an action before it should be send.
    Literal {
        data: Vec<u8>,
        #[cfg(feature = "ext_literal")]
        #[cfg_attr(docsrs, doc(cfg(feature = "ext_literal")))]
        mode: LiteralMode,
    },
}

//--------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EncodeContext {
    accumulator: Vec<u8>,
    items: Vec<Fragment>,
}

impl EncodeContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_line(&mut self) {
        self.items.push(Fragment::Line {
            data: std::mem::take(&mut self.accumulator),
        })
    }

    pub fn push_literal(&mut self, #[cfg(feature = "ext_literal")] mode: LiteralMode) {
        self.items.push(Fragment::Literal {
            data: std::mem::take(&mut self.accumulator),
            #[cfg(feature = "ext_literal")]
            mode,
        })
    }

    pub fn into_items(self) -> Vec<Fragment> {
        let Self {
            accumulator,
            mut items,
        } = self;

        if !accumulator.is_empty() {
            items.push(Fragment::Line { data: accumulator });
        }

        items
    }

    pub fn dump(self) -> Vec<u8> {
        let mut out = Vec::new();

        for item in self.into_items() {
            match item {
                Fragment::Line { data } | Fragment::Literal { data, .. } => {
                    out.extend_from_slice(&data)
                }
            }
        }

        out
    }
}

impl Write for EncodeContext {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.accumulator.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<T> Encode for T
where
    T: Encoder,
{
    fn encode(&self) -> Encoded {
        let mut encode_context = EncodeContext::new();
        T::encode_ctx(self, &mut encode_context).unwrap();

        Encoded {
            items: encode_context.into_items(),
        }
    }
}

// -------------------------------------------------------------------------------------------------

pub trait Encoder {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()>;
}

// ----- Primitive ---------------------------------------------------------------------------------

impl Encoder for u32 {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(self.to_string().as_bytes())
    }
}

impl Encoder for u64 {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(self.to_string().as_bytes())
    }
}

// ----- Command -----------------------------------------------------------------------------------

impl<'a> Encoder for Command<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        self.tag.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.body.encode_ctx(ctx)?;
        ctx.write_all(b"\r\n")
    }
}

impl<'a> Encoder for Tag<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(self.inner().as_bytes())
    }
}

impl<'a> Encoder for CommandBody<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            CommandBody::Capability => ctx.write_all(b"CAPABILITY"),
            CommandBody::Noop => ctx.write_all(b"NOOP"),
            CommandBody::Logout => ctx.write_all(b"LOGOUT"),
            #[cfg(feature = "starttls")]
            CommandBody::StartTLS => ctx.write_all(b"STARTTLS"),
            CommandBody::Authenticate {
                mechanism,
                #[cfg(feature = "ext_sasl_ir")]
                initial_response,
            } => {
                ctx.write_all(b"AUTHENTICATE")?;
                ctx.write_all(b" ")?;
                mechanism.encode_ctx(ctx)?;

                #[cfg(feature = "ext_sasl_ir")]
                if let Some(ir) = initial_response {
                    ctx.write_all(b" ")?;

                    // RFC 4959 (https://datatracker.ietf.org/doc/html/rfc4959#section-3)
                    // "To send a zero-length initial response, the client MUST send a single pad character ("=").
                    // This indicates that the response is present, but is a zero-length string."
                    if ir.declassify().is_empty() {
                        ctx.write_all(b"=")?;
                    } else {
                        ctx.write_all(base64.encode(ir.declassify()).as_bytes())?;
                    };
                };

                Ok(())
            }
            CommandBody::Login { username, password } => {
                ctx.write_all(b"LOGIN")?;
                ctx.write_all(b" ")?;
                username.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                password.declassify().encode_ctx(ctx)
            }
            CommandBody::Select { mailbox } => {
                ctx.write_all(b"SELECT")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)
            }
            #[cfg(feature = "ext_unselect")]
            CommandBody::Unselect => ctx.write_all(b"UNSELECT"),
            CommandBody::Examine { mailbox } => {
                ctx.write_all(b"EXAMINE")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)
            }
            CommandBody::Create { mailbox } => {
                ctx.write_all(b"CREATE")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)
            }
            CommandBody::Delete { mailbox } => {
                ctx.write_all(b"DELETE")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)
            }
            CommandBody::Rename {
                from: mailbox,
                to: new_mailbox,
            } => {
                ctx.write_all(b"RENAME")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                new_mailbox.encode_ctx(ctx)
            }
            CommandBody::Subscribe { mailbox } => {
                ctx.write_all(b"SUBSCRIBE")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)
            }
            CommandBody::Unsubscribe { mailbox } => {
                ctx.write_all(b"UNSUBSCRIBE")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)
            }
            CommandBody::List {
                reference,
                mailbox_wildcard,
            } => {
                ctx.write_all(b"LIST")?;
                ctx.write_all(b" ")?;
                reference.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                mailbox_wildcard.encode_ctx(ctx)
            }
            CommandBody::Lsub {
                reference,
                mailbox_wildcard,
            } => {
                ctx.write_all(b"LSUB")?;
                ctx.write_all(b" ")?;
                reference.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                mailbox_wildcard.encode_ctx(ctx)
            }
            CommandBody::Status {
                mailbox,
                item_names,
            } => {
                ctx.write_all(b"STATUS")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                ctx.write_all(b"(")?;
                join_serializable(item_names, b" ", ctx)?;
                ctx.write_all(b")")
            }
            CommandBody::Append {
                mailbox,
                flags,
                date,
                message,
            } => {
                ctx.write_all(b"APPEND")?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)?;

                if !flags.is_empty() {
                    ctx.write_all(b" ")?;
                    ctx.write_all(b"(")?;
                    join_serializable(flags, b" ", ctx)?;
                    ctx.write_all(b")")?;
                }

                if let Some(date) = date {
                    ctx.write_all(b" ")?;
                    date.encode_ctx(ctx)?;
                }

                ctx.write_all(b" ")?;
                message.encode_ctx(ctx)
            }
            CommandBody::Check => ctx.write_all(b"CHECK"),
            CommandBody::Close => ctx.write_all(b"CLOSE"),
            CommandBody::Expunge => ctx.write_all(b"EXPUNGE"),
            CommandBody::Search {
                charset,
                criteria,
                uid,
            } => {
                if *uid {
                    ctx.write_all(b"UID SEARCH")?;
                } else {
                    ctx.write_all(b"SEARCH")?;
                }
                if let Some(charset) = charset {
                    ctx.write_all(b" CHARSET ")?;
                    charset.encode_ctx(ctx)?;
                }
                ctx.write_all(b" ")?;
                criteria.encode_ctx(ctx)
            }
            CommandBody::Fetch {
                sequence_set,
                macro_or_item_names,
                uid,
            } => {
                if *uid {
                    ctx.write_all(b"UID FETCH ")?;
                } else {
                    ctx.write_all(b"FETCH ")?;
                }

                sequence_set.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                macro_or_item_names.encode_ctx(ctx)
            }
            CommandBody::Store {
                sequence_set,
                kind,
                response,
                flags,
                uid,
            } => {
                if *uid {
                    ctx.write_all(b"UID STORE ")?;
                } else {
                    ctx.write_all(b"STORE ")?;
                }

                sequence_set.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;

                match kind {
                    StoreType::Add => ctx.write_all(b"+")?,
                    StoreType::Remove => ctx.write_all(b"-")?,
                    StoreType::Replace => {}
                }

                ctx.write_all(b"FLAGS")?;

                match response {
                    StoreResponse::Answer => {}
                    StoreResponse::Silent => ctx.write_all(b".SILENT")?,
                }

                ctx.write_all(b" (")?;
                join_serializable(flags, b" ", ctx)?;
                ctx.write_all(b")")
            }
            CommandBody::Copy {
                sequence_set,
                mailbox,
                uid,
            } => {
                if *uid {
                    ctx.write_all(b"UID COPY ")?;
                } else {
                    ctx.write_all(b"COPY ")?;
                }
                sequence_set.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)
            }
            #[cfg(feature = "ext_idle")]
            CommandBody::Idle => ctx.write_all(b"IDLE"),
            #[cfg(feature = "ext_enable")]
            CommandBody::Enable { capabilities } => {
                ctx.write_all(b"ENABLE ")?;
                join_serializable(capabilities.as_ref(), b" ", ctx)
            }
            #[cfg(feature = "ext_compress")]
            CommandBody::Compress { algorithm } => {
                ctx.write_all(b"COMPRESS ")?;
                algorithm.encode_ctx(ctx)
            }
            #[cfg(feature = "ext_quota")]
            CommandBody::GetQuota { root } => {
                ctx.write_all(b"GETQUOTA ")?;
                root.encode_ctx(ctx)
            }
            #[cfg(feature = "ext_quota")]
            CommandBody::GetQuotaRoot { mailbox } => {
                ctx.write_all(b"GETQUOTAROOT ")?;
                mailbox.encode_ctx(ctx)
            }
            #[cfg(feature = "ext_quota")]
            CommandBody::SetQuota { root, quotas } => {
                ctx.write_all(b"SETQUOTA ")?;
                root.encode_ctx(ctx)?;
                ctx.write_all(b" (")?;
                join_serializable(quotas.as_ref(), b" ", ctx)?;
                ctx.write_all(b")")
            }
            #[cfg(feature = "ext_move")]
            CommandBody::Move {
                sequence_set,
                mailbox,
                uid,
            } => {
                if *uid {
                    ctx.write_all(b"UID MOVE ")?;
                } else {
                    ctx.write_all(b"MOVE ")?;
                }
                sequence_set.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)
            }
        }
    }
}

impl<'a> Encoder for AuthMechanism<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        write!(ctx, "{}", self)
    }
}

impl Encoder for AuthenticateData {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        let encoded = base64.encode(self.0.declassify());
        ctx.write_all(encoded.as_bytes())?;
        ctx.write_all(b"\r\n")
    }
}

impl<'a> Encoder for AString<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            AString::Atom(atom) => atom.encode_ctx(ctx),
            AString::String(imap_str) => imap_str.encode_ctx(ctx),
        }
    }
}

impl<'a> Encoder for Atom<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(self.inner().as_bytes())
    }
}

impl<'a> Encoder for AtomExt<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(self.inner().as_bytes())
    }
}

impl<'a> Encoder for IString<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::Literal(val) => val.encode_ctx(ctx),
            Self::Quoted(val) => val.encode_ctx(ctx),
        }
    }
}

impl<'a> Encoder for Literal<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        #[cfg(not(feature = "ext_literal"))]
        write!(ctx, "{{{}}}\r\n", self.as_ref().len())?;

        #[cfg(feature = "ext_literal")]
        match self.mode() {
            LiteralMode::Sync => write!(ctx, "{{{}}}\r\n", self.as_ref().len())?,
            LiteralMode::NonSync => write!(ctx, "{{{}+}}\r\n", self.as_ref().len())?,
        }

        ctx.push_line();

        ctx.write_all(self.as_ref())?;

        #[cfg(not(feature = "ext_literal"))]
        ctx.push_literal();
        #[cfg(feature = "ext_literal")]
        ctx.push_literal(self.mode());

        Ok(())
    }
}

impl<'a> Encoder for Quoted<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        write!(ctx, "\"{}\"", escape_quoted(self.inner()))
    }
}

impl<'a> Encoder for Mailbox<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Mailbox::Inbox => ctx.write_all(b"INBOX"),
            Mailbox::Other(other) => other.encode_ctx(ctx),
        }
    }
}

impl<'a> Encoder for MailboxOther<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        self.inner().encode_ctx(ctx)
    }
}

impl<'a> Encoder for ListMailbox<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            ListMailbox::Token(lcs) => lcs.encode_ctx(ctx),
            ListMailbox::String(istr) => istr.encode_ctx(ctx),
        }
    }
}

impl<'a> Encoder for ListCharString<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(self.as_ref())
    }
}

impl Encoder for StatusDataItemName {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::Messages => ctx.write_all(b"MESSAGES"),
            Self::Recent => ctx.write_all(b"RECENT"),
            Self::UidNext => ctx.write_all(b"UIDNEXT"),
            Self::UidValidity => ctx.write_all(b"UIDVALIDITY"),
            Self::Unseen => ctx.write_all(b"UNSEEN"),
            #[cfg(feature = "ext_quota")]
            Self::Deleted => ctx.write_all(b"DELETED"),
            #[cfg(feature = "ext_quota")]
            Self::DeletedStorage => ctx.write_all(b"DELETED-STORAGE"),
            #[cfg(feature = "ext_condstore_qresync")]
            Self::HighestModSeq => ctx.write_all(b"HIGHESTMODSEQ"),
        }
    }
}

impl<'a> Encoder for Flag<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Flag::Seen => ctx.write_all(b"\\Seen"),
            Flag::Answered => ctx.write_all(b"\\Answered"),
            Flag::Flagged => ctx.write_all(b"\\Flagged"),
            Flag::Deleted => ctx.write_all(b"\\Deleted"),
            Flag::Draft => ctx.write_all(b"\\Draft"),
            Flag::Extension(other) => other.encode_ctx(ctx),
            Flag::Keyword(atom) => atom.encode_ctx(ctx),
        }
    }
}

impl<'a> Encoder for FlagFetch<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::Flag(flag) => flag.encode_ctx(ctx),
            Self::Recent => ctx.write_all(b"\\Recent"),
        }
    }
}

impl<'a> Encoder for FlagPerm<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::Flag(flag) => flag.encode_ctx(ctx),
            Self::AllowNewKeywords => ctx.write_all(b"\\*"),
        }
    }
}

impl<'a> Encoder for FlagExtension<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(b"\\")?;
        ctx.write_all(self.as_ref().as_bytes())
    }
}

impl Encoder for DateTime {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        self.as_ref().encode_ctx(ctx)
    }
}

impl<'a> Encoder for Charset<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Charset::Atom(atom) => atom.encode_ctx(ctx),
            Charset::Quoted(quoted) => quoted.encode_ctx(ctx),
        }
    }
}

impl<'a> Encoder for SearchKey<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            SearchKey::All => ctx.write_all(b"ALL"),
            SearchKey::Answered => ctx.write_all(b"ANSWERED"),
            SearchKey::Bcc(astring) => {
                ctx.write_all(b"BCC ")?;
                astring.encode_ctx(ctx)
            }
            SearchKey::Before(date) => {
                ctx.write_all(b"BEFORE ")?;
                date.encode_ctx(ctx)
            }
            SearchKey::Body(astring) => {
                ctx.write_all(b"BODY ")?;
                astring.encode_ctx(ctx)
            }
            SearchKey::Cc(astring) => {
                ctx.write_all(b"CC ")?;
                astring.encode_ctx(ctx)
            }
            SearchKey::Deleted => ctx.write_all(b"DELETED"),
            SearchKey::Flagged => ctx.write_all(b"FLAGGED"),
            SearchKey::From(astring) => {
                ctx.write_all(b"FROM ")?;
                astring.encode_ctx(ctx)
            }
            SearchKey::Keyword(flag_keyword) => {
                ctx.write_all(b"KEYWORD ")?;
                flag_keyword.encode_ctx(ctx)
            }
            SearchKey::New => ctx.write_all(b"NEW"),
            SearchKey::Old => ctx.write_all(b"OLD"),
            SearchKey::On(date) => {
                ctx.write_all(b"ON ")?;
                date.encode_ctx(ctx)
            }
            SearchKey::Recent => ctx.write_all(b"RECENT"),
            SearchKey::Seen => ctx.write_all(b"SEEN"),
            SearchKey::Since(date) => {
                ctx.write_all(b"SINCE ")?;
                date.encode_ctx(ctx)
            }
            SearchKey::Subject(astring) => {
                ctx.write_all(b"SUBJECT ")?;
                astring.encode_ctx(ctx)
            }
            SearchKey::Text(astring) => {
                ctx.write_all(b"TEXT ")?;
                astring.encode_ctx(ctx)
            }
            SearchKey::To(astring) => {
                ctx.write_all(b"TO ")?;
                astring.encode_ctx(ctx)
            }
            SearchKey::Unanswered => ctx.write_all(b"UNANSWERED"),
            SearchKey::Undeleted => ctx.write_all(b"UNDELETED"),
            SearchKey::Unflagged => ctx.write_all(b"UNFLAGGED"),
            SearchKey::Unkeyword(flag_keyword) => {
                ctx.write_all(b"UNKEYWORD ")?;
                flag_keyword.encode_ctx(ctx)
            }
            SearchKey::Unseen => ctx.write_all(b"UNSEEN"),
            SearchKey::Draft => ctx.write_all(b"DRAFT"),
            SearchKey::Header(header_fld_name, astring) => {
                ctx.write_all(b"HEADER ")?;
                header_fld_name.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                astring.encode_ctx(ctx)
            }
            SearchKey::Larger(number) => write!(ctx, "LARGER {number}"),
            SearchKey::Not(search_key) => {
                ctx.write_all(b"NOT ")?;
                search_key.encode_ctx(ctx)
            }
            SearchKey::Or(search_key_a, search_key_b) => {
                ctx.write_all(b"OR ")?;
                search_key_a.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                search_key_b.encode_ctx(ctx)
            }
            SearchKey::SentBefore(date) => {
                ctx.write_all(b"SENTBEFORE ")?;
                date.encode_ctx(ctx)
            }
            SearchKey::SentOn(date) => {
                ctx.write_all(b"SENTON ")?;
                date.encode_ctx(ctx)
            }
            SearchKey::SentSince(date) => {
                ctx.write_all(b"SENTSINCE ")?;
                date.encode_ctx(ctx)
            }
            SearchKey::Smaller(number) => write!(ctx, "SMALLER {number}"),
            SearchKey::Uid(sequence_set) => {
                ctx.write_all(b"UID ")?;
                sequence_set.encode_ctx(ctx)
            }
            SearchKey::Undraft => ctx.write_all(b"UNDRAFT"),
            SearchKey::SequenceSet(sequence_set) => sequence_set.encode_ctx(ctx),
            SearchKey::And(search_keys) => {
                ctx.write_all(b"(")?;
                join_serializable(search_keys.as_ref(), b" ", ctx)?;
                ctx.write_all(b")")
            }
        }
    }
}

impl Encoder for SequenceSet {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        join_serializable(self.0.as_ref(), b",", ctx)
    }
}

impl Encoder for Sequence {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Sequence::Single(seq_no) => seq_no.encode_ctx(ctx),
            Sequence::Range(from, to) => {
                from.encode_ctx(ctx)?;
                ctx.write_all(b":")?;
                to.encode_ctx(ctx)
            }
        }
    }
}

impl Encoder for SeqOrUid {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            SeqOrUid::Value(number) => write!(ctx, "{number}"),
            SeqOrUid::Asterisk => ctx.write_all(b"*"),
        }
    }
}

impl Encoder for NaiveDate {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        write!(ctx, "\"{}\"", self.as_ref().format("%d-%b-%Y"))
    }
}

impl<'a> Encoder for MacroOrMessageDataItemNames<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::Macro(m) => m.encode_ctx(ctx),
            Self::MessageDataItemNames(item_names) => {
                if item_names.len() == 1 {
                    item_names[0].encode_ctx(ctx)
                } else {
                    ctx.write_all(b"(")?;
                    join_serializable(item_names.as_slice(), b" ", ctx)?;
                    ctx.write_all(b")")
                }
            }
        }
    }
}

impl Encoder for Macro {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        write!(ctx, "{}", self)
    }
}

impl<'a> Encoder for MessageDataItemName<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::Body => ctx.write_all(b"BODY"),
            Self::BodyExt {
                section,
                partial,
                peek,
            } => {
                if *peek {
                    ctx.write_all(b"BODY.PEEK[")?;
                } else {
                    ctx.write_all(b"BODY[")?;
                }
                if let Some(section) = section {
                    section.encode_ctx(ctx)?;
                }
                ctx.write_all(b"]")?;
                if let Some((a, b)) = partial {
                    write!(ctx, "<{a}.{b}>")?;
                }

                Ok(())
            }
            Self::BodyStructure => ctx.write_all(b"BODYSTRUCTURE"),
            Self::Envelope => ctx.write_all(b"ENVELOPE"),
            Self::Flags => ctx.write_all(b"FLAGS"),
            Self::InternalDate => ctx.write_all(b"INTERNALDATE"),
            Self::Rfc822 => ctx.write_all(b"RFC822"),
            Self::Rfc822Header => ctx.write_all(b"RFC822.HEADER"),
            Self::Rfc822Size => ctx.write_all(b"RFC822.SIZE"),
            Self::Rfc822Text => ctx.write_all(b"RFC822.TEXT"),
            Self::Uid => ctx.write_all(b"UID"),
        }
    }
}

impl<'a> Encoder for Section<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Section::Part(part) => part.encode_ctx(ctx),
            Section::Header(maybe_part) => match maybe_part {
                Some(part) => {
                    part.encode_ctx(ctx)?;
                    ctx.write_all(b".HEADER")
                }
                None => ctx.write_all(b"HEADER"),
            },
            Section::HeaderFields(maybe_part, header_list) => {
                match maybe_part {
                    Some(part) => {
                        part.encode_ctx(ctx)?;
                        ctx.write_all(b".HEADER.FIELDS (")?;
                    }
                    None => ctx.write_all(b"HEADER.FIELDS (")?,
                };
                join_serializable(header_list.as_ref(), b" ", ctx)?;
                ctx.write_all(b")")
            }
            Section::HeaderFieldsNot(maybe_part, header_list) => {
                match maybe_part {
                    Some(part) => {
                        part.encode_ctx(ctx)?;
                        ctx.write_all(b".HEADER.FIELDS.NOT (")?;
                    }
                    None => ctx.write_all(b"HEADER.FIELDS.NOT (")?,
                };
                join_serializable(header_list.as_ref(), b" ", ctx)?;
                ctx.write_all(b")")
            }
            Section::Text(maybe_part) => match maybe_part {
                Some(part) => {
                    part.encode_ctx(ctx)?;
                    ctx.write_all(b".TEXT")
                }
                None => ctx.write_all(b"TEXT"),
            },
            Section::Mime(part) => {
                part.encode_ctx(ctx)?;
                ctx.write_all(b".MIME")
            }
        }
    }
}

impl Encoder for Part {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        join_serializable(self.0.as_ref(), b".", ctx)
    }
}

impl Encoder for NonZeroU32 {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        write!(ctx, "{self}")
    }
}

impl<'a> Encoder for Capability<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        write!(ctx, "{}", self)
    }
}

// ----- Responses ---------------------------------------------------------------------------------

impl<'a> Encoder for Response<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Response::Status(status) => status.encode_ctx(ctx),
            Response::Data(data) => data.encode_ctx(ctx),
            Response::Continue(continue_request) => continue_request.encode_ctx(ctx),
        }
    }
}

impl<'a> Encoder for Greeting<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(b"* ")?;
        self.kind.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;

        if let Some(ref code) = self.code {
            ctx.write_all(b"[")?;
            code.encode_ctx(ctx)?;
            ctx.write_all(b"] ")?;
        }

        self.text.encode_ctx(ctx)?;
        ctx.write_all(b"\r\n")
    }
}

impl Encoder for GreetingKind {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            GreetingKind::Ok => ctx.write_all(b"OK"),
            GreetingKind::PreAuth => ctx.write_all(b"PREAUTH"),
            GreetingKind::Bye => ctx.write_all(b"BYE"),
        }
    }
}

impl<'a> Encoder for Status<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        fn format_status(
            tag: &Option<Tag>,
            status: &str,
            code: &Option<Code>,
            comment: &Text,
            ctx: &mut EncodeContext,
        ) -> std::io::Result<()> {
            match tag {
                Some(tag) => tag.encode_ctx(ctx)?,
                None => ctx.write_all(b"*")?,
            }
            ctx.write_all(b" ")?;
            ctx.write_all(status.as_bytes())?;
            ctx.write_all(b" ")?;
            if let Some(code) = code {
                ctx.write_all(b"[")?;
                code.encode_ctx(ctx)?;
                ctx.write_all(b"] ")?;
            }
            comment.encode_ctx(ctx)?;
            ctx.write_all(b"\r\n")
        }

        match self {
            Status::Ok { tag, code, text } => format_status(tag, "OK", code, text, ctx),
            Status::No { tag, code, text } => format_status(tag, "NO", code, text, ctx),
            Status::Bad { tag, code, text } => format_status(tag, "BAD", code, text, ctx),
            Status::Bye { code, text } => format_status(&None, "BYE", code, text, ctx),
        }
    }
}

impl<'a> Encoder for Code<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Code::Alert => ctx.write_all(b"ALERT"),
            Code::BadCharset { allowed } => {
                if allowed.is_empty() {
                    ctx.write_all(b"BADCHARSET")
                } else {
                    ctx.write_all(b"BADCHARSET (")?;
                    join_serializable(allowed, b" ", ctx)?;
                    ctx.write_all(b")")
                }
            }
            Code::Capability(caps) => {
                ctx.write_all(b"CAPABILITY ")?;
                join_serializable(caps.as_ref(), b" ", ctx)
            }
            Code::Parse => ctx.write_all(b"PARSE"),
            Code::PermanentFlags(flags) => {
                ctx.write_all(b"PERMANENTFLAGS (")?;
                join_serializable(flags, b" ", ctx)?;
                ctx.write_all(b")")
            }
            Code::ReadOnly => ctx.write_all(b"READ-ONLY"),
            Code::ReadWrite => ctx.write_all(b"READ-WRITE"),
            Code::TryCreate => ctx.write_all(b"TRYCREATE"),
            Code::UidNext(next) => {
                ctx.write_all(b"UIDNEXT ")?;
                next.encode_ctx(ctx)
            }
            Code::UidValidity(validity) => {
                ctx.write_all(b"UIDVALIDITY ")?;
                validity.encode_ctx(ctx)
            }
            Code::Unseen(seq) => {
                ctx.write_all(b"UNSEEN ")?;
                seq.encode_ctx(ctx)
            }
            // RFC 2221
            #[cfg(any(feature = "ext_login_referrals", feature = "ext_mailbox_referrals"))]
            Code::Referral(url) => {
                ctx.write_all(b"REFERRAL ")?;
                ctx.write_all(url.as_bytes())
            }
            #[cfg(feature = "ext_compress")]
            Code::CompressionActive => ctx.write_all(b"COMPRESSIONACTIVE"),
            #[cfg(feature = "ext_quota")]
            Code::OverQuota => ctx.write_all(b"OVERQUOTA"),
            #[cfg(feature = "ext_literal")]
            Code::TooBig => ctx.write_all(b"TOOBIG"),
            Code::Other(unknown) => unknown.encode_ctx(ctx),
        }
    }
}

impl<'a> Encoder for CodeOther<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(self.inner())
    }
}

impl<'a> Encoder for Text<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(self.inner().as_bytes())
    }
}

impl<'a> Encoder for Data<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Data::Capability(caps) => {
                ctx.write_all(b"* CAPABILITY ")?;
                join_serializable(caps.as_ref(), b" ", ctx)?;
            }
            Data::List {
                items,
                delimiter,
                mailbox,
            } => {
                ctx.write_all(b"* LIST (")?;
                join_serializable(items, b" ", ctx)?;
                ctx.write_all(b") ")?;

                if let Some(delimiter) = delimiter {
                    ctx.write_all(b"\"")?;
                    delimiter.encode_ctx(ctx)?;
                    ctx.write_all(b"\"")?;
                } else {
                    ctx.write_all(b"NIL")?;
                }
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)?;
            }
            Data::Lsub {
                items,
                delimiter,
                mailbox,
            } => {
                ctx.write_all(b"* LSUB (")?;
                join_serializable(items, b" ", ctx)?;
                ctx.write_all(b") ")?;

                if let Some(delimiter) = delimiter {
                    ctx.write_all(b"\"")?;
                    delimiter.encode_ctx(ctx)?;
                    ctx.write_all(b"\"")?;
                } else {
                    ctx.write_all(b"NIL")?;
                }
                ctx.write_all(b" ")?;
                mailbox.encode_ctx(ctx)?;
            }
            Data::Status { mailbox, items } => {
                ctx.write_all(b"* STATUS ")?;
                mailbox.encode_ctx(ctx)?;
                ctx.write_all(b" (")?;
                join_serializable(items, b" ", ctx)?;
                ctx.write_all(b")")?;
            }
            Data::Search(seqs) => {
                if seqs.is_empty() {
                    ctx.write_all(b"* SEARCH")?;
                } else {
                    ctx.write_all(b"* SEARCH ")?;
                    join_serializable(seqs, b" ", ctx)?;
                }
            }
            Data::Flags(flags) => {
                ctx.write_all(b"* FLAGS (")?;
                join_serializable(flags, b" ", ctx)?;
                ctx.write_all(b")")?;
            }
            Data::Exists(count) => write!(ctx, "* {count} EXISTS")?,
            Data::Recent(count) => write!(ctx, "* {count} RECENT")?,
            Data::Expunge(msg) => write!(ctx, "* {msg} EXPUNGE")?,
            Data::Fetch { seq, items } => {
                write!(ctx, "* {seq} FETCH (")?;
                join_serializable(items.as_ref(), b" ", ctx)?;
                ctx.write_all(b")")?;
            }
            #[cfg(feature = "ext_enable")]
            Data::Enabled { capabilities } => {
                write!(ctx, "* ENABLED")?;

                for cap in capabilities {
                    ctx.write_all(b" ")?;
                    cap.encode_ctx(ctx)?;
                }
            }
            #[cfg(feature = "ext_quota")]
            Data::Quota { root, quotas } => {
                ctx.write_all(b"* QUOTA ")?;
                root.encode_ctx(ctx)?;
                ctx.write_all(b" (")?;
                join_serializable(quotas.as_ref(), b" ", ctx)?;
                ctx.write_all(b")")?;
            }
            #[cfg(feature = "ext_quota")]
            Data::QuotaRoot { mailbox, roots } => {
                ctx.write_all(b"* QUOTAROOT ")?;
                mailbox.encode_ctx(ctx)?;
                for root in roots {
                    ctx.write_all(b" ")?;
                    root.encode_ctx(ctx)?;
                }
            }
        }

        ctx.write_all(b"\r\n")
    }
}

impl<'a> Encoder for FlagNameAttribute<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::Noinferiors => ctx.write_all(b"\\Noinferiors"),
            Self::Noselect => ctx.write_all(b"\\Noselect"),
            Self::Marked => ctx.write_all(b"\\Marked"),
            Self::Unmarked => ctx.write_all(b"\\Unmarked"),
            Self::Extension(atom) => {
                ctx.write_all(b"\\")?;
                atom.encode_ctx(ctx)
            }
        }
    }
}

impl Encoder for QuotedChar {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self.inner() {
            '\\' => ctx.write_all(b"\\\\"),
            '"' => ctx.write_all(b"\\\""),
            other => ctx.write_all(&[other as u8]),
        }
    }
}

impl Encoder for StatusDataItem {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::Messages(count) => {
                ctx.write_all(b"MESSAGES ")?;
                count.encode_ctx(ctx)
            }
            Self::Recent(count) => {
                ctx.write_all(b"RECENT ")?;
                count.encode_ctx(ctx)
            }
            Self::UidNext(next) => {
                ctx.write_all(b"UIDNEXT ")?;
                next.encode_ctx(ctx)
            }
            Self::UidValidity(identifier) => {
                ctx.write_all(b"UIDVALIDITY ")?;
                identifier.encode_ctx(ctx)
            }
            Self::Unseen(count) => {
                ctx.write_all(b"UNSEEN ")?;
                count.encode_ctx(ctx)
            }
            #[cfg(feature = "ext_quota")]
            Self::Deleted(count) => {
                ctx.write_all(b"DELETED ")?;
                count.encode_ctx(ctx)
            }
            #[cfg(feature = "ext_quota")]
            Self::DeletedStorage(count) => {
                ctx.write_all(b"DELETED-STORAGE ")?;
                count.encode_ctx(ctx)
            }
        }
    }
}

impl<'a> Encoder for MessageDataItem<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Self::BodyExt {
                section,
                origin,
                data,
            } => {
                ctx.write_all(b"BODY[")?;
                if let Some(section) = section {
                    section.encode_ctx(ctx)?;
                }
                ctx.write_all(b"]")?;
                if let Some(origin) = origin {
                    write!(ctx, "<{origin}>")?;
                }
                ctx.write_all(b" ")?;
                data.encode_ctx(ctx)
            }
            // FIXME: do not return body-ext-1part and body-ext-mpart here
            Self::Body(body) => {
                ctx.write_all(b"BODY ")?;
                body.encode_ctx(ctx)
            }
            Self::BodyStructure(body) => {
                ctx.write_all(b"BODYSTRUCTURE ")?;
                body.encode_ctx(ctx)
            }
            Self::Envelope(envelope) => {
                ctx.write_all(b"ENVELOPE ")?;
                envelope.encode_ctx(ctx)
            }
            Self::Flags(flags) => {
                ctx.write_all(b"FLAGS (")?;
                join_serializable(flags, b" ", ctx)?;
                ctx.write_all(b")")
            }
            Self::InternalDate(datetime) => {
                ctx.write_all(b"INTERNALDATE ")?;
                datetime.encode_ctx(ctx)
            }
            Self::Rfc822(nstring) => {
                ctx.write_all(b"RFC822 ")?;
                nstring.encode_ctx(ctx)
            }
            Self::Rfc822Header(nstring) => {
                ctx.write_all(b"RFC822.HEADER ")?;
                nstring.encode_ctx(ctx)
            }
            Self::Rfc822Size(size) => write!(ctx, "RFC822.SIZE {size}"),
            Self::Rfc822Text(nstring) => {
                ctx.write_all(b"RFC822.TEXT ")?;
                nstring.encode_ctx(ctx)
            }
            Self::Uid(uid) => write!(ctx, "UID {uid}"),
        }
    }
}

impl<'a> Encoder for NString<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match &self.0 {
            Some(imap_str) => imap_str.encode_ctx(ctx),
            None => ctx.write_all(b"NIL"),
        }
    }
}

impl<'a> Encoder for BodyStructure<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(b"(")?;
        match self {
            BodyStructure::Single {
                body,
                extension_data: extension,
            } => {
                body.encode_ctx(ctx)?;
                if let Some(extension) = extension {
                    ctx.write_all(b" ")?;
                    extension.encode_ctx(ctx)?;
                }
            }
            BodyStructure::Multi {
                bodies,
                subtype,
                extension_data,
            } => {
                for body in bodies.as_ref() {
                    body.encode_ctx(ctx)?;
                }
                ctx.write_all(b" ")?;
                subtype.encode_ctx(ctx)?;

                if let Some(extension) = extension_data {
                    ctx.write_all(b" ")?;
                    extension.encode_ctx(ctx)?;
                }
            }
        }
        ctx.write_all(b")")
    }
}

impl<'a> Encoder for Body<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self.specific {
            SpecificFields::Basic {
                r#type: ref type_,
                ref subtype,
            } => {
                type_.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                subtype.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                self.basic.encode_ctx(ctx)
            }
            SpecificFields::Message {
                ref envelope,
                ref body_structure,
                number_of_lines,
            } => {
                ctx.write_all(b"\"MESSAGE\" \"RFC822\" ")?;
                self.basic.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                envelope.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                body_structure.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                write!(ctx, "{number_of_lines}")
            }
            SpecificFields::Text {
                ref subtype,
                number_of_lines,
            } => {
                ctx.write_all(b"\"TEXT\" ")?;
                subtype.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                self.basic.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                write!(ctx, "{number_of_lines}")
            }
        }
    }
}

impl<'a> Encoder for BasicFields<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        List1AttributeValueOrNil(&self.parameter_list).encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.id.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.description.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.content_transfer_encoding.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        write!(ctx, "{}", self.size)
    }
}

impl<'a> Encoder for Envelope<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(b"(")?;
        self.date.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.subject.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        List1OrNil(&self.from, b"").encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        List1OrNil(&self.sender, b"").encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        List1OrNil(&self.reply_to, b"").encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        List1OrNil(&self.to, b"").encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        List1OrNil(&self.cc, b"").encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        List1OrNil(&self.bcc, b"").encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.in_reply_to.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.message_id.encode_ctx(ctx)?;
        ctx.write_all(b")")
    }
}

impl<'a> Encoder for Address<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        ctx.write_all(b"(")?;
        self.name.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.adl.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.mailbox.encode_ctx(ctx)?;
        ctx.write_all(b" ")?;
        self.host.encode_ctx(ctx)?;
        ctx.write_all(b")")?;

        Ok(())
    }
}

impl<'a> Encoder for SinglePartExtensionData<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        self.md5.encode_ctx(ctx)?;

        if let Some(disposition) = &self.tail {
            ctx.write_all(b" ")?;
            disposition.encode_ctx(ctx)?;
        }

        Ok(())
    }
}

impl<'a> Encoder for MultiPartExtensionData<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        List1AttributeValueOrNil(&self.parameter_list).encode_ctx(ctx)?;

        if let Some(disposition) = &self.tail {
            ctx.write_all(b" ")?;
            disposition.encode_ctx(ctx)?;
        }

        Ok(())
    }
}

impl<'a> Encoder for Disposition<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match &self.disposition {
            Some((s, param)) => {
                ctx.write_all(b"(")?;
                s.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                List1AttributeValueOrNil(param).encode_ctx(ctx)?;
                ctx.write_all(b")")?;
            }
            None => ctx.write_all(b"NIL")?,
        }

        if let Some(language) = &self.tail {
            ctx.write_all(b" ")?;
            language.encode_ctx(ctx)?;
        }

        Ok(())
    }
}

impl<'a> Encoder for Language<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        List1OrNil(&self.language, b" ").encode_ctx(ctx)?;

        if let Some(location) = &self.tail {
            ctx.write_all(b" ")?;
            location.encode_ctx(ctx)?;
        }

        Ok(())
    }
}

impl<'a> Encoder for Location<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        self.location.encode_ctx(ctx)?;

        for body_extension in &self.extensions {
            ctx.write_all(b" ")?;
            body_extension.encode_ctx(ctx)?;
        }

        Ok(())
    }
}

impl<'a> Encoder for BodyExtension<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            BodyExtension::NString(nstring) => nstring.encode_ctx(ctx),
            BodyExtension::Number(number) => number.encode_ctx(ctx),
            BodyExtension::List(list) => {
                ctx.write_all(b"(")?;
                join_serializable(list.as_ref(), b" ", ctx)?;
                ctx.write_all(b")")
            }
        }
    }
}

impl Encoder for ChronoDateTime<FixedOffset> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        write!(ctx, "\"{}\"", self.format("%d-%b-%Y %H:%M:%S %z"))
    }
}

impl<'a> Encoder for Continue<'a> {
    fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
        match self {
            Continue::Basic(continue_basic) => match continue_basic.code() {
                Some(code) => {
                    ctx.write_all(b"+ [")?;
                    code.encode_ctx(ctx)?;
                    ctx.write_all(b"] ")?;
                    continue_basic.text().encode_ctx(ctx)?;
                    ctx.write_all(b"\r\n")
                }
                None => {
                    ctx.write_all(b"+ ")?;
                    continue_basic.text().encode_ctx(ctx)?;
                    ctx.write_all(b"\r\n")
                }
            },
            // TODO: Is this correct when data is empty?
            Continue::Base64(data) => {
                ctx.write_all(b"+ ")?;
                ctx.write_all(base64.encode(data).as_bytes())?;
                ctx.write_all(b"\r\n")
            }
        }
    }
}

mod utils {
    use std::io::Write;

    use super::Encoder;
    use crate::codec::encode::EncodeContext;

    pub struct List1OrNil<'a, T>(pub &'a Vec<T>, pub &'a [u8]);

    pub struct List1AttributeValueOrNil<'a, T>(pub &'a Vec<(T, T)>);

    pub fn join_serializable<I: Encoder>(
        elements: &[I],
        sep: &[u8],
        ctx: &mut EncodeContext,
    ) -> std::io::Result<()> {
        if let Some((last, head)) = elements.split_last() {
            for item in head {
                item.encode_ctx(ctx)?;
                ctx.write_all(sep)?;
            }

            last.encode_ctx(ctx)
        } else {
            Ok(())
        }
    }

    impl<'a, T> Encoder for List1OrNil<'a, T>
    where
        T: Encoder,
    {
        fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
            if let Some((last, head)) = self.0.split_last() {
                ctx.write_all(b"(")?;

                for item in head {
                    item.encode_ctx(ctx)?;
                    ctx.write_all(self.1)?;
                }

                last.encode_ctx(ctx)?;

                ctx.write_all(b")")
            } else {
                ctx.write_all(b"NIL")
            }
        }
    }

    impl<'a, T> Encoder for List1AttributeValueOrNil<'a, T>
    where
        T: Encoder,
    {
        fn encode_ctx(&self, ctx: &mut EncodeContext) -> std::io::Result<()> {
            if let Some((last, head)) = self.0.split_last() {
                ctx.write_all(b"(")?;

                for (attribute, value) in head {
                    attribute.encode_ctx(ctx)?;
                    ctx.write_all(b" ")?;
                    value.encode_ctx(ctx)?;
                    ctx.write_all(b" ")?;
                }

                let (attribute, value) = last;
                attribute.encode_ctx(ctx)?;
                ctx.write_all(b" ")?;
                value.encode_ctx(ctx)?;

                ctx.write_all(b")")
            } else {
                ctx.write_all(b"NIL")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use imap_types::{
        auth::AuthMechanism,
        command::{Command, CommandBody},
        core::{AString, Literal, NString, NonEmptyVec},
        fetch::MessageDataItem,
        response::{Data, Response},
        utils::escape_byte_string,
    };

    use super::*;

    #[test]
    fn test_api_encoder_usage() {
        let cmd = Command::new(
            "A",
            CommandBody::login(
                AString::from(
                    #[cfg(not(feature = "ext_literal"))]
                    Literal::unvalidated(b"alice".as_ref()),
                    #[cfg(feature = "ext_literal")]
                    Literal::unvalidated_non_sync(b"alice".as_ref()),
                ),
                "password",
            )
            .unwrap(),
        )
        .unwrap();

        // Dump.
        let got_encoded = cmd.encode().dump();

        // Encoding.
        let encoder = cmd.encode();

        let mut out = Vec::new();

        for x in encoder {
            match x {
                Fragment::Line { data } => {
                    println!("C: {}", escape_byte_string(&data));
                    out.extend_from_slice(&data);
                }
                #[cfg(not(feature = "ext_literal"))]
                Fragment::Literal { data } => {
                    println!("C: <Waiting for continuation request>");

                    println!("C: {}", escape_byte_string(&data));
                    out.extend_from_slice(&data);
                }
                #[cfg(feature = "ext_literal")]
                Fragment::Literal { data, mode } => {
                    match mode {
                        LiteralMode::Sync => println!("C: <Waiting for continuation request>"),
                        LiteralMode::NonSync => println!("C: <Skipped continuation request>"),
                    }

                    println!("C: {}", escape_byte_string(&data));
                    out.extend_from_slice(&data);
                }
            }
        }

        assert_eq!(got_encoded, out);
    }

    #[test]
    fn test_encode_command() {
        kat_encoder(&[
            (
                Command::new("A", CommandBody::login("alice", "pass").unwrap()).unwrap(),
                [Fragment::Line {
                    data: b"A LOGIN alice pass\r\n".to_vec(),
                }]
                .as_ref(),
            ),
            (
                Command::new(
                    "A",
                    CommandBody::login("alice", b"\xCA\xFE".as_ref()).unwrap(),
                )
                .unwrap(),
                [
                    Fragment::Line {
                        data: b"A LOGIN alice {2}\r\n".to_vec(),
                    },
                    Fragment::Literal {
                        data: b"\xCA\xFE".to_vec(),
                        #[cfg(feature = "ext_literal")]
                        mode: LiteralMode::Sync,
                    },
                    Fragment::Line {
                        data: b"\r\n".to_vec(),
                    },
                ]
                .as_ref(),
            ),
            (
                Command::new("A", CommandBody::authenticate(AuthMechanism::Login)).unwrap(),
                [Fragment::Line {
                    data: b"A AUTHENTICATE LOGIN\r\n".to_vec(),
                }]
                .as_ref(),
            ),
            #[cfg(feature = "ext_sasl_ir")]
            (
                Command::new(
                    "A",
                    CommandBody::authenticate_with_ir(AuthMechanism::Login, b"alice".as_ref()),
                )
                .unwrap(),
                [Fragment::Line {
                    data: b"A AUTHENTICATE LOGIN YWxpY2U=\r\n".to_vec(),
                }]
                .as_ref(),
            ),
            (
                Command::new("A", CommandBody::authenticate(AuthMechanism::Plain)).unwrap(),
                [Fragment::Line {
                    data: b"A AUTHENTICATE PLAIN\r\n".to_vec(),
                }]
                .as_ref(),
            ),
            #[cfg(feature = "ext_sasl_ir")]
            (
                Command::new(
                    "A",
                    CommandBody::authenticate_with_ir(
                        AuthMechanism::Plain,
                        b"\x00alice\x00pass".as_ref(),
                    ),
                )
                .unwrap(),
                [Fragment::Line {
                    data: b"A AUTHENTICATE PLAIN AGFsaWNlAHBhc3M=\r\n".to_vec(),
                }]
                .as_ref(),
            ),
        ]);
    }

    #[test]
    fn test_encode_response() {
        kat_encoder(&[
            (
                Response::Data(Data::Fetch {
                    seq: NonZeroU32::new(12345).unwrap(),
                    items: NonEmptyVec::from(MessageDataItem::BodyExt {
                        section: None,
                        origin: None,
                        data: NString::from(Literal::unvalidated(b"ABCDE".as_ref())),
                    }),
                }),
                [
                    Fragment::Line {
                        data: b"* 12345 FETCH (BODY[] {5}\r\n".to_vec(),
                    },
                    Fragment::Literal {
                        data: b"ABCDE".to_vec(),
                        #[cfg(feature = "ext_literal")]
                        mode: LiteralMode::Sync,
                    },
                    Fragment::Line {
                        data: b")\r\n".to_vec(),
                    },
                ]
                .as_ref(),
            ),
            #[cfg(feature = "ext_literal")]
            (
                Response::Data(Data::Fetch {
                    seq: NonZeroU32::new(12345).unwrap(),
                    items: NonEmptyVec::from(MessageDataItem::BodyExt {
                        section: None,
                        origin: None,
                        data: NString::from(Literal::unvalidated_non_sync(b"ABCDE".as_ref())),
                    }),
                }),
                [
                    Fragment::Line {
                        data: b"* 12345 FETCH (BODY[] {5+}\r\n".to_vec(),
                    },
                    Fragment::Literal {
                        data: b"ABCDE".to_vec(),
                        mode: LiteralMode::NonSync,
                    },
                    Fragment::Line {
                        data: b")\r\n".to_vec(),
                    },
                ]
                .as_ref(),
            ),
        ])
    }

    fn kat_encoder<Object, Actions>(tests: &[(Object, Actions)])
    where
        Object: Encode,
        Actions: AsRef<[Fragment]>,
    {
        for (i, (obj, actions)) in tests.iter().enumerate() {
            println!("# Testing {i}");

            let encoder = obj.encode();
            let actions = actions.as_ref();

            assert_eq!(encoder.collect::<Vec<_>>(), actions);
        }
    }
}