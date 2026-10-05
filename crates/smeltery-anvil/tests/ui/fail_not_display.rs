use serde::Serialize;
use smeltery_anvil::BroadcastEvent;

#[derive(Serialize)]
struct Order;

#[derive(Serialize, BroadcastEvent)]
#[broadcast(crate = "smeltery_anvil", private = "orders.{order}")]
struct OrderShipped {
    order: Order,
}

fn main() {}
