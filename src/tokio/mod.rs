use std::io::Error;

use bytes::{Buf, BufMut, BytesMut};
use imap_types::{
    bounded_static::IntoBoundedStatic, codec::Encode, command::Command, response::Response,
};
use tokio_util::codec::{Decoder, Encoder};

use crate::command::command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapServerCodec {
    state: State,
    max_literal_size: usize,
}

/// All interactions transmitted by client and server are in the form of
/// lines, that is, strings that end with a CRLF.
///
/// The protocol receiver of an IMAP4rev1 client or server is either ...
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// ... reading a line, or ...
    ReadLine { to_consume_acc: usize },
    /// ... is reading a sequence of octets
    /// with a known count followed by a line.
    ReadLiteral { to_consume_acc: usize, needed: u32 },
}

impl ImapServerCodec {
    pub fn new(max_literal_size: usize) -> Self {
        Self {
            state: State::ReadLine { to_consume_acc: 0 },
            max_literal_size,
        }
    }

    fn find_crlf_inclusive(skip: usize, buf: &BytesMut) -> Result<Option<usize>, LineKind> {
        match buf.iter().skip(skip).position(|item| *item == b'\n') {
            Some(position) => {
                if buf[skip + position.saturating_sub(1)] != b'\r' {
                    Err(LineKind::NotCrLf)
                } else {
                    Ok(Some(position + 1))
                }
            }
            None => Ok(None),
        }
    }

    fn parse_literal(line: &[u8]) -> Result<Option<u32>, LiteralKind> {
        match Self::parse_literal_enclosing(line) {
            Ok(maybe_raw) => {
                if let Some(raw) = maybe_raw {
                    let str = std::str::from_utf8(raw).map_err(|_| LiteralKind::BadNumber)?;
                    let num = u32::from_str_radix(str, 10).map_err(|_| LiteralKind::BadNumber)?;

                    Ok(Some(num))
                } else {
                    Ok(None)
                }
            }
            Err(err) => Err(err),
        }
    }

    fn parse_literal_enclosing(line: &[u8]) -> Result<Option<&[u8]>, LiteralKind> {
        if line.len() == 0 {
            return Ok(None);
        }

        if line[line.len() - 1] != b'}' {
            return Ok(None);
        }

        let mut index = line.len() - 1;

        while index > 0 {
            index -= 1;

            if line[index] == b'{' {
                return Ok(Some(&line[index + 1..line.len() - 1]));
            }
        }

        return Err(LiteralKind::NoOpeningBrace);
    }
}

#[derive(Debug)]
pub enum ImapServerCodecError {
    Io(std::io::Error),
    Line(LineKind),
    Literal(LiteralKind),
    CommandParsingFailed,
    ActionRequired,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LineKind {
    NotCrLf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LiteralKind {
    TooLarge(u32),
    BadNumber,
    NoOpeningBrace,
}

impl PartialEq for ImapServerCodecError {
    fn eq(&self, other: &Self) -> bool {
        use ImapServerCodecError::*;

        match (self, other) {
            (Io(error1), Io(error2)) => error1.kind() == error2.kind(),
            (Line(kind2), Line(kind1)) => kind1 == kind2,
            (Literal(kind1), Literal(kind2)) => kind1 == kind2,
            (CommandParsingFailed, CommandParsingFailed) => true,
            (ActionRequired, ActionRequired) => true,
            _ => false,
        }
    }
}

impl From<std::io::Error> for ImapServerCodecError {
    fn from(error: Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Command(Command<'static>),
    ActionRequired(Action),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    SendLiteralAck(u32),
    SendLiteralReject(u32),
}

impl Decoder for ImapServerCodec {
    type Item = Outcome;
    type Error = ImapServerCodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match self.state {
                State::ReadLine {
                    ref mut to_consume_acc,
                } => {
                    match ImapServerCodec::find_crlf_inclusive(*to_consume_acc, src) {
                        Ok(Some(to_consume)) => {
                            *to_consume_acc += to_consume;

                            match ImapServerCodec::parse_literal(&src[..*to_consume_acc - 2]) {
                                // No literal.
                                Ok(None) => match command(&src[..*to_consume_acc]) {
                                    Ok((rem, cmd)) => {
                                        assert!(rem.is_empty());
                                        let cmd = cmd.into_static();

                                        src.advance(*to_consume_acc);
                                        self.state = State::ReadLine { to_consume_acc: 0 };

                                        return Ok(Some(Outcome::Command(cmd)));
                                    }
                                    Err(_) => {
                                        src.advance(*to_consume_acc);

                                        return Err(ImapServerCodecError::CommandParsingFailed);
                                    }
                                },
                                // Literal found.
                                Ok(Some(needed)) => {
                                    if self.max_literal_size < needed as usize {
                                        src.advance(*to_consume_acc);
                                        self.state = State::ReadLine { to_consume_acc: 0 };

                                        return Ok(Some(Outcome::ActionRequired(
                                            Action::SendLiteralReject(needed),
                                        )));
                                    }

                                    src.reserve(needed as usize);

                                    self.state = State::ReadLiteral {
                                        to_consume_acc: *to_consume_acc,
                                        needed,
                                    };

                                    return Ok(Some(Outcome::ActionRequired(
                                        Action::SendLiteralAck(needed),
                                    )));
                                }
                                // Error processing literal.
                                Err(error) => {
                                    src.clear();
                                    self.state = State::ReadLine { to_consume_acc: 0 };

                                    return Err(ImapServerCodecError::Literal(error));
                                }
                            }
                        }
                        // More data needed.
                        Ok(None) => {
                            return Ok(None);
                        }
                        // Error processing newline.
                        Err(error) => {
                            src.clear();
                            self.state = State::ReadLine { to_consume_acc: 0 };

                            return Err(ImapServerCodecError::Line(error));
                        }
                    }
                }
                State::ReadLiteral {
                    to_consume_acc,
                    needed,
                } => {
                    if to_consume_acc + needed as usize <= src.len() {
                        self.state = State::ReadLine {
                            to_consume_acc: to_consume_acc + needed as usize,
                        }
                    } else {
                        return Ok(None);
                    }
                }
            }
        }
    }
}

impl<'a> Encoder<Response<'a>> for ImapServerCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Response, dst: &mut BytesMut) -> Result<(), std::io::Error> {
        //dst.reserve(item.len());
        let mut writer = dst.writer();
        item.encode(&mut writer).unwrap();
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::convert::TryFrom;

    use bytes::BytesMut;
    use imap_types::{
        command::{Command, CommandBody},
        core::{AString, AtomExt, IString, Literal, Tag},
    };
    use tokio_util::codec::Decoder;

    use crate::tokio::{Action, ImapServerCodec, ImapServerCodecError, LineKind, Outcome};

    #[test]
    fn find_crlf_inclusive() {
        let tests = [
            (b"A\r".as_ref(), 0, Ok(None)),
            (b"A\r\n", 0, Ok(Some(3))),
            (b"A\n", 0, Err(LineKind::NotCrLf)),
            (b"\n", 0, Err(LineKind::NotCrLf)),
            (b"aaa\r\nA\r".as_ref(), 5, Ok(None)),
            (b"aaa\r\nA\r\n", 5, Ok(Some(3))),
            (b"aaa\r\nA\n", 5, Err(LineKind::NotCrLf)),
            (b"aaa\r\n\n", 5, Err(LineKind::NotCrLf)),
        ];

        for (test, skip, expected) in tests {
            let bytes = BytesMut::from(test);

            let got = ImapServerCodec::find_crlf_inclusive(skip, &bytes);

            dbg!((std::str::from_utf8(test).unwrap(), skip, &expected, &got));

            assert_eq!(expected, got);
        }
    }

    #[test]
    fn decoder_line() {
        let tests = [
            (b"".as_ref(), Ok(None)),
            (b"a noop", Ok(None)),
            (b"\r", Ok(None)),
            (
                b"\n",
                Ok(Some(Outcome::Command(Command::new(
                    Tag::try_from("a").unwrap(),
                    CommandBody::Noop,
                )))),
            ),
            (b"", Ok(None)),
            (b"xxxx", Ok(None)),
            (b"\r\n", Err(ImapServerCodecError::CommandParsingFailed)),
        ];

        let mut src = BytesMut::new();
        let mut codec = ImapServerCodec::new(1024);

        for (test, expected) in tests {
            src.extend_from_slice(test);
            let got = codec.decode(&mut src);

            assert_eq!(expected, got);

            dbg!((std::str::from_utf8(test).unwrap(), &expected, &got));
        }
    }

    #[test]
    fn decoder_literal() {
        let tests = [
            (b"".as_ref(), Ok(None)),
            (b"a login", Ok(None)),
            (b" {", Ok(None)),
            (b"5", Ok(None)),
            (b"}", Ok(None)),
            (
                b"\r\n",
                Ok(Some(Outcome::ActionRequired(Action::SendLiteralAck(5)))),
            ),
            (b"a", Ok(None)),
            (b"l", Ok(None)),
            (b"i", Ok(None)),
            (b"ce", Ok(None)),
            (b" ", Ok(None)),
            (
                b"password\r\n",
                Ok(Some(Outcome::Command(Command::new(
                    Tag::try_from("a").unwrap(),
                    CommandBody::Login {
                        username: AString::String(IString::Literal(
                            Literal::try_from(b"alice".as_ref()).unwrap(),
                        )),
                        password: AString::Atom(AtomExt::try_from("password").unwrap()),
                    },
                )))),
            ),
        ];

        let mut src = BytesMut::new();
        let mut codec = ImapServerCodec::new(1024);

        for (test, expected) in tests {
            src.extend_from_slice(test);
            let got = codec.decode(&mut src);

            dbg!((std::str::from_utf8(test).unwrap(), &expected, &got));

            assert_eq!(expected, got);
        }
    }

    #[test]
    fn decoder_error() {
        let tests = [
            (
                b"xxx\r\n".as_ref(),
                Err(ImapServerCodecError::CommandParsingFailed),
            ),
            (
                b"a noop\r\n",
                Ok(Some(Outcome::Command(Command::new(
                    Tag::try_from("a").unwrap(),
                    CommandBody::Noop,
                )))),
            ),
        ];

        let mut src = BytesMut::new();
        let mut codec = ImapServerCodec::new(1024);

        for (test, expected) in tests {
            src.extend_from_slice(test);
            let got = codec.decode(&mut src);

            dbg!((std::str::from_utf8(test).unwrap(), &expected, &got));

            assert_eq!(expected, got);
        }
    }
}