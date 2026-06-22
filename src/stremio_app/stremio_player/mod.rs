pub mod player;
pub use player::Player;
pub mod communication;
pub use communication::{
    CmdVal, InMsg, InMsgArgs, InMsgFn, MpvCmd, PlayerEnded, PlayerEvent, PlayerProprChange,
    PlayerResponse, PropKey, PropVal,
};
#[cfg(test)]
mod communication_tests;
