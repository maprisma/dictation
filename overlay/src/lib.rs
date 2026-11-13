//! Maprisma Dictation: the top-centre voice and typing overlay (step 4, docs/overlay.md). It
//! listens through maprisma-voice (com.maprisma.Voice1), asks the shared AI helper and shows the
//! answer, with the aura ring around the pill.

pub mod aura;
pub mod corners;
pub mod helper;
pub mod instance;
pub mod last;
pub mod look;
pub mod markdown;
pub mod paths;
pub mod settings;
pub mod shared_ai;
pub mod shot;
pub mod state;
pub mod ui;
pub mod voice;
