use serde::Serialize;
use smeltery_anvil::{BroadcastEvent, Channel};

#[derive(Serialize, BroadcastEvent)]
#[broadcast(crate = "smeltery_anvil", private = "orders.{order_id}", public = "orders")]
struct OrderShipped {
    order_id: i64,
}

fn main() {
    let event = OrderShipped { order_id: 7 };
    assert_eq!(event.channels(), vec![Channel::private("orders.7"), Channel::public("orders")]);
    assert_eq!(event.name(), r"App\Events\OrderShipped");
}
