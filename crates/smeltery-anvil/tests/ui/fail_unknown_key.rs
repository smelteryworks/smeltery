use serde::Serialize;
use smeltery_anvil::BroadcastEvent;

#[derive(Serialize, BroadcastEvent)]
#[broadcast(crate = "smeltery_anvil", secret = "rooms.{room}")]
struct Joined {
    room: i64,
}

fn main() {}
