use serde::Serialize;
use smeltery_anvil::BroadcastEvent;

#[derive(Serialize, BroadcastEvent)]
#[broadcast(crate = "smeltery_anvil", private = "orders.{order}")]
struct OrderShipped {
    order_id: i64,
}

fn main() {}
