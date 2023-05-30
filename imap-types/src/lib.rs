//! # Misuse-resistant Types for the IMAP Protocol
//!
//! The main types in imap-types are [Greeting](response::Greeting), [Command](command::Command), and [Response](response::Response), and we use the term "message" to refer to either of them.
//!
//! ## Module structure
//!
//! The module structure reflects this terminology:
//! types that are specific to commands are in the [command](command) module;
//! types that are specific to responses (including the greeting) are in the [response](response) module;
//! types used in both are in the [message](message) module.
//! The [codec](codec) module contains the [Decode](codec::Decode) trait used to serialize messages.
//! The [core] module contains "string types" -- there should be no need to use them directly.
//!
//! ## Simple construction of messages.
//!
//! Messages can be created in different ways.
//! However, what all ways have in common is, that the API does not allow the creation of invalid ones.
//!
//! For example, all commands in IMAP (and many responses) are prefixed with a "tag".
//! Although IMAP tags are just strings, they have additional rules, such as that no whitespace is allowed.
//! Thus, imap-codec encapsulates tags in the [Tag](message::Tag) struct and ensures no invalid tag can be created.
//! This is why [Result](std::result::Result) is often used in associated functions or methods.
//!
//! Generally, imap-codec relies a lot on the [From](std::convert::From), [TryFrom](std::convert::TryFrom), [Into](std::convert::Into), and [TryInto](std::convert::TryInto) traits.
//! Make good use of them.
//! For types that are more cumbersome to create, there are helper methods available.
//!
//! ### Example
//!
//! ```
//! use imap_types::{
//!     command::{Command, CommandBody},
//!     message::Tag,
//! };
//!
//! // # Variant 1
//! // Create a `Command` with `tag` "A123" and `body` "NOOP".
//! // (Note: `Command::new()` returns `Result::Err(...)` when the tag is invalid.)
//! let cmd = Command::new("A123", CommandBody::Noop).unwrap();
//!
//! // # Variant 2
//! // Create a `CommandBody` first and finalize it into
//! // a `Command` by attaching a tag later.
//! let cmd = CommandBody::Noop.tag("A123").unwrap();
//!
//! // # Variant 3
//! // Create a `Command` directly.
//! let cmd = Command {
//!     tag: Tag::try_from("A123").unwrap(),
//!     body: CommandBody::Noop,
//! };
//! ```
//!
//! ## More complex messages.
//!
//! ### Example
//!
//! The following example is a server fetch response containing the size and MIME structure of message 42.
//!
//! ```
//! use std::{borrow::Cow, num::NonZeroU32};
//!
//! use imap_types::{
//!     core::{IString, NString, NonEmptyVec},
//!     response::{
//!         data::{
//!             BasicFields, Body, BodyStructure, FetchAttributeValue, SinglePartExtensionData,
//!             SpecificFields,
//!         },
//!         Data, Response,
//!     },
//! };
//!
//! let fetch = {
//!     let data = Data::Fetch {
//!         seq_or_uid: NonZeroU32::new(42).unwrap(),
//!         attributes: NonEmptyVec::try_from(vec![
//!             FetchAttributeValue::Rfc822Size(1337),
//!             FetchAttributeValue::Body(BodyStructure::Single {
//!                 body: Body {
//!                     basic: BasicFields {
//!                         parameter_list: vec![],
//!                         id: NString(None),
//!                         description: NString(Some(
//!                             IString::try_from("Important message.").unwrap(),
//!                         )),
//!                         content_transfer_encoding: IString::try_from("base64").unwrap(),
//!                         size: 512,
//!                     },
//!                     specific: SpecificFields::Basic {
//!                         type_: IString::try_from("text").unwrap(),
//!                         subtype: IString::try_from("html").unwrap(),
//!                     },
//!                 },
//!                 extension_data: None,
//!             }),
//!         ])
//!         .unwrap(),
//!     };
//!
//!     Response::Data(data)
//! };
//! ```
//!
//! # A Note on Types
//!
//! Due to the correctness guarantees, this library uses multiple "string types" like `Atom`, `Tag`, `NString`, and `IString`. See the [core](core) module.

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

#[cfg(feature = "arbitrary")]
mod arbitrary;
mod imap4rev1;
pub mod secret;
pub mod utils;

// -- API -----------------------------------------------------------------------------------

pub mod state;

pub mod core {
    //! # Core Data Types
    //!
    //! This module exposes IMAPs "core data types" (or "string types").
    //! It is loosely based on the IMAP standard.
    //! Some additional types are defined and some might be missing.
    //!
    //! "IMAP4rev1 uses textual commands and responses.
    //! Data in IMAP4rev1 can be in one of several forms: atom, number, string, parenthesized list, or NIL.
    //! Note that a particular data item may take more than one form; for example, a data item defined as using "astring" syntax may be either an atom or a string." ([RFC 3501](https://www.rfc-editor.org/rfc/rfc3501.html))
    //!
    //! ## (Incomplete) Summary
    //!
    //! ```text
    //!        ┌───────┐ ┌─────────────────┐
    //!        │AString│ │     NString     │
    //!        └──┬─┬──┘ │(Option<IString>)│
    //!           │ │    └─────┬───────────┘
    //!           │ └──────┐   │
    //!           │        │   │
    //! ┌────┐ ┌──▼────┐ ┌─▼───▼─┐
    //! │Atom│ │AtomExt│ │IString│
    //! └────┘ └───────┘ └┬─────┬┘
    //!                   │     │
    //!             ┌─────▼─┐ ┌─▼────┐
    //!             │Literal│ │Quoted│
    //!             └───────┘ └──────┘
    //! ```

    pub use crate::imap4rev1::core::{
        AString, Atom, AtomError, AtomExt, AtomExtError, IString, Literal, LiteralError, NString,
        NonEmptyVec, NonEmptyVecError, Quoted, QuotedCharError, QuotedError, TagError, TextError,
    };
}

pub mod message {
    //! # Types used in commands and responses

    pub use crate::imap4rev1::{
        core::{Charset, Tag},
        datetime::{DateTime, NaiveDate},
        flag::{Flag, FlagError, FlagExtension, FlagFetch, FlagNameAttribute, FlagPerm},
        mailbox::{Mailbox, MailboxOther},
        section::{Part, PartSpecifier, Section},
        AuthMechanism, AuthMechanismOther,
    };
}

pub mod command {
    //! # Types used in commands

    pub use crate::imap4rev1::{
        command::{
            AppendError, AuthenticateData, Command, CommandBody, CopyError, ListError, LoginError,
            RenameError,
        },
        mailbox::{ListCharString, ListMailbox},
        sequence::{SeqOrUid, Sequence, SequenceSet, SequenceSetError, Strategy},
    };

    pub mod status {
        //! # Types used in STATUS command

        pub use crate::imap4rev1::status_attributes::StatusAttribute;
    }

    pub mod search {
        //! # Types used in SEARCH command

        pub use crate::imap4rev1::command::search::SearchKey;
    }

    pub mod fetch {
        //! # Types used in FETCH command

        pub use crate::imap4rev1::fetch_attributes::{
            FetchAttribute, Macro, MacroOrFetchAttributes,
        };
    }

    pub mod store {
        //! # Types used in STORE command
        pub use crate::imap4rev1::flag::{StoreResponse, StoreType};
    }
}

pub mod response {
    //! # Types used in responses

    pub use crate::imap4rev1::{
        core::Text,
        response::{
            Code, CodeOther, Continue, ContinueBasic, Data, Greeting, GreetingKind, Response,
            Status,
        },
    };

    pub mod data {
        pub use crate::imap4rev1::{
            address::Address,
            body::{
                BasicFields, Body, BodyExtension, BodyStructure, Disposition, Language, Location,
                MultiPartExtensionData, SinglePartExtensionData, SpecificFields,
            },
            core::QuotedChar,
            envelope::Envelope,
            fetch_attributes::FetchAttributeValue,
            flag::FlagNameAttribute,
            response::{Capability, CapabilityOther},
            status_attributes::StatusAttributeValue,
        };
    }
}

#[cfg(any(
    feature = "ext_compress",
    feature = "ext_enable",
    feature = "ext_idle",
    feature = "ext_literal",
    feature = "ext_move",
    feature = "ext_quota",
    feature = "ext_unselect",
))]
pub mod extensions;

// -- Re-exports -----------------------------------------------------------------------------------

#[cfg(feature = "bounded-static")]
pub use bounded_static;
