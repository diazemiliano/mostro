use anyhow::Result;
use easy_hasher::easy_hasher::*;
use lightning_invoice::Invoice;
use log::info;
use nostr::hashes::hex::FromHex;
use nostr::hashes::hex::ToHex;
use rand::RngCore;
use std::env;
use std::str::FromStr;
use tonic_openssl_lnd::invoicesrpc::{
    AddHoldInvoiceRequest, AddHoldInvoiceResp, CancelInvoiceMsg, CancelInvoiceResp,
    SettleInvoiceMsg, SettleInvoiceResp,
};
use tonic_openssl_lnd::lnrpc::invoice::InvoiceState;
use tonic_openssl_lnd::lnrpc::payment::PaymentStatus;
use tonic_openssl_lnd::routerrpc::{SendPaymentRequest, TrackPaymentRequest};
use tonic_openssl_lnd::{LndClient, LndClientError};

pub struct LndConnector {
    client: LndClient,
}

#[derive(Debug, Clone)]
pub struct InvoiceMessage {
    pub hash: Vec<u8>,
    pub state: InvoiceState,
}

impl LndConnector {
    pub async fn new() -> Self {
        let port: u32 = env::var("LND_GRPC_PORT")
            .expect("LND_GRPC_PORT must be set")
            .parse()
            .expect("port is not u32");
        let host = env::var("LND_GRPC_HOST").expect("LND_GRPC_HOST must be set");
        let tls_path = env::var("LND_CERT_FILE").expect("LND_CERT_FILE must be set");
        let macaroon_path = env::var("LND_MACAROON_FILE").expect("LND_MACAROON_FILE must be set");

        // Connecting to LND requires only host, port, cert file, and macaroon file
        let client = tonic_openssl_lnd::connect(host, port, tls_path, macaroon_path)
            .await
            .expect("Failed connecting to LND");

        Self { client }
    }

    pub async fn create_hold_invoice(
        &mut self,
        description: &str,
        amount: i64,
    ) -> Result<(AddHoldInvoiceResp, Vec<u8>, Vec<u8>), LndClientError> {
        let mut preimage = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut preimage);
        let hash = raw_sha256(preimage.to_vec());

        let invoice = AddHoldInvoiceRequest {
            hash: hash.to_vec(),
            memo: description.to_string(),
            value: amount,
            ..Default::default()
        };
        let holdinvoice = self
            .client
            .invoices()
            .add_hold_invoice(invoice)
            .await
            .expect("Failed to add hold invoice")
            .into_inner();

        Ok((holdinvoice, preimage.to_vec(), hash.to_vec()))
    }

    pub async fn subscribe_invoice(
        &mut self,
        r_hash: Vec<u8>,
        listener: tokio::sync::mpsc::Sender<InvoiceMessage>,
    ) {
        let mut invoice_stream = self
            .client
            .invoices()
            .subscribe_single_invoice(
                tonic_openssl_lnd::invoicesrpc::SubscribeSingleInvoiceRequest {
                    r_hash: r_hash.clone(),
                },
            )
            .await
            .expect("Failed to call subscribe_single_invoice")
            .into_inner();

        while let Some(invoice) = invoice_stream
            .message()
            .await
            .expect("Failed to receive invoices")
        {
            if let Some(state) =
                tonic_openssl_lnd::lnrpc::invoice::InvoiceState::from_i32(invoice.state)
            {
                let msg = InvoiceMessage {
                    hash: r_hash.clone(),
                    state,
                };
                listener
                    .clone()
                    .send(msg)
                    .await
                    .expect("Failed to send a message");
            }
        }
    }

    pub async fn settle_hold_invoice(
        &mut self,
        preimage: &str,
    ) -> Result<SettleInvoiceResp, LndClientError> {
        let preimage = FromHex::from_hex(preimage).expect("Wrong preimage");

        let preimage_message = SettleInvoiceMsg { preimage };
        let settle = self
            .client
            .invoices()
            .settle_invoice(preimage_message)
            .await
            .expect("Failed to settle hold invoice")
            .into_inner();

        Ok(settle)
    }

    pub async fn cancel_hold_invoice(
        &mut self,
        hash: &str,
    ) -> Result<CancelInvoiceResp, LndClientError> {
        let payment_hash = FromHex::from_hex(hash).expect("Wrong payment hash");

        let cancel_message = CancelInvoiceMsg { payment_hash };
        let cancel = self
            .client
            .invoices()
            .cancel_invoice(cancel_message)
            .await
            .expect("Failed to cancel hold invoice")
            .into_inner();

        Ok(cancel)
    }

    pub async fn send_payment(&mut self, payment_request: &str, amount: i64) {
        let invoice = decode_invoice(payment_request).unwrap();

        let payment_hash = invoice.payment_hash();
        let payment_hash = payment_hash.to_vec();
        let hash = payment_hash.to_hex();

        let track_payment_req = TrackPaymentRequest {
            payment_hash,
            no_inflight_updates: true,
        };
        let track = self
            .client
            .router()
            .track_payment_v2(track_payment_req)
            .await;
        // We only send the payment if it wasn't attempted before
        if track.is_ok() {
            info!("Aborting paying invoice with hash {} to buyer", hash);
            return;
        }

        let invoice_amount_milli = invoice.amount_milli_satoshis();
        let mut request = SendPaymentRequest {
            payment_request: payment_request.to_string(),
            timeout_seconds: 60,
            ..Default::default()
        };
        // We add amount to the request only if the invoice doesn't have amount
        if invoice_amount_milli.is_none() {
            request = SendPaymentRequest {
                amt: amount,
                ..request
            };
        }
        println!("request => {request:#?}");
        let mut stream = self
            .client
            .router()
            .send_payment_v2(request)
            .await
            .expect("Failed sending payment")
            .into_inner();

        while let Some(payment) = stream.message().await.expect("Failed paying invoice") {
            if let Some(status) = PaymentStatus::from_i32(payment.status) {
                if status == PaymentStatus::Succeeded {
                    info!("Invoice with hash: {hash} paid!");
                    // TODO: send messages to parties
                    // update order record
                }
            }
        }
    }
}

/// Decode a lightning invoice (bolt11)
pub fn decode_invoice(payment_request: &str) -> Result<Invoice> {
    let invoice = Invoice::from_str(payment_request)?;

    Ok(invoice)
}
