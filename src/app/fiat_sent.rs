use crate::messages;
use crate::util::{send_dm, update_order_event};

use anyhow::Result;
use log::error;
use mostro_core::order::Order;
use mostro_core::{Action, Content, Message, Peer, Status};
use nostr_sdk::prelude::*;
use sqlx::{Pool, Sqlite};
use sqlx_crud::Crud;

pub async fn fiat_sent_action(
    msg: Message,
    event: &Event,
    my_keys: &Keys,
    client: &Client,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let order_id = msg.order_id.unwrap();
    let order = match Order::by_id(pool, order_id).await? {
        Some(order) => order,
        None => {
            error!("FiatSent: Order Id {order_id} not found!");
            return Ok(());
        }
    };
    // TODO: send to user a DM with the error
    if order.status != "Active" {
        error!("FiatSent: Order Id {order_id} wrong status");
        return Ok(());
    }
    // Check if the pubkey is the buyer
    if Some(event.pubkey.to_bech32()?) != order.buyer_pubkey {
        let text_message = messages::cant_do();
        // We create a Message
        let message = Message::new(
            0,
            Some(order.id),
            Action::CantDo,
            Some(Content::TextMessage(text_message)),
        );
        let message = message.as_json()?;
        send_dm(client, my_keys, &event.pubkey, message).await?;
    }

    // We publish a new replaceable kind nostr event with the status updated
    // and update on local database the status and new event id
    update_order_event(pool, client, my_keys, Status::FiatSent, &order, None).await?;

    let seller_pubkey = match order.seller_pubkey.as_ref() {
        Some(pk) => XOnlyPublicKey::from_bech32(pk)?,
        None => {
            error!("Seller pubkey not found for order {}!", order.id);
            return Ok(());
        }
    };
    let peer = Peer::new(event.pubkey.to_bech32()?);

    // We create a Message
    let message = Message::new(
        0,
        Some(order.id),
        Action::FiatSent,
        Some(Content::Peer(peer)),
    );
    let message = message.as_json().unwrap();
    send_dm(client, my_keys, &seller_pubkey, message).await?;
    // We send a message to buyer to wait
    let peer = Peer::new(seller_pubkey.to_bech32()?);

    // We create a Message
    let message = Message::new(
        0,
        Some(order.id),
        Action::FiatSent,
        Some(Content::Peer(peer)),
    );
    let message = message.as_json()?;
    send_dm(client, my_keys, &event.pubkey, message).await?;
    Ok(())
}
