//! Keeps NVIDIA Instant Replay recording by patching two checks inside the
//! ShadowPlay nvcontainer.exe process. See re_research.md for the details.

#![cfg(windows)]

pub mod memory;
pub mod patch;
