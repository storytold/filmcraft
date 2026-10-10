//! Text front end for FilmCraft's Text to Speech (phase P2a).
//!
//! [`normalize`] turns a narration script into plain spoken words, structured as
//! [`Token`]s: words, pauses ([`Token::Pause`], from `[pause 1s]`-style markers),
//! sentence ends ([`Token::SentenceEnd`]) and kept punctuation ([`Token::Punct`]).
//! It handles numbers (cardinals, ordinals, negatives, decimals), money (`$`, `€`,
//! `£`), percent, years, clock times, dates, phone-like digit groups, common
//! abbreviations, acronyms and the `&`/`+`/`@`/`#`/`/` symbols — English
//! ([`Lang::EnUs`] / [`Lang::EnGb`]).
//!
//! This crate is the first stage of the text pipeline; the grapheme-to-phoneme step
//! (P2b) consumes its words. It is pure Rust, depends only on `serde` and
//! `thiserror`, and never crashes on hostile input (see `AGENTS.md` §0): input over
//! 1 MiB is a [`NormalizeError::TooLong`], oversized numbers are read digit by digit,
//! and malformed pause markers fall back to plain text rather than erroring.
//!
//! The rules were written from scratch (clean-room); see the crate README for the
//! judgement calls and limitations.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod normalize;
pub mod phonemize;

pub use normalize::{Lang, NormalizeError, Token, normalize};
pub use phonemize::{Chunk, LexError, Lexicon, MAX_CHUNK_CHARS, Overrides, SYMBOLS, phonemize};
