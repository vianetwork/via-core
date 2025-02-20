use std::{env, str::FromStr};

use bitcoin::{Address as BitcoinAddress, Network};
use musig2::KeyAggContext;
use rand::rngs::OsRng;
use secp256k1_musig2::{PublicKey, Secp256k1, SecretKey};

#[derive(Debug)]
#[allow(dead_code)]
struct ContributorOutput {
    secret_key: Option<SecretKey>,
    public_key: PublicKey,
}

#[derive(Debug)]
#[allow(dead_code)]
struct CoordinatorOutput {
    participant_count: usize,
    bridge_address: BitcoinAddress,
}

fn generate_keypair() -> (SecretKey, PublicKey) {
    let mut rng = OsRng;
    let secp = Secp256k1::new();
    let secret_key = SecretKey::new(&mut rng);
    let public_key = PublicKey::from_secret_key(&secp, &secret_key);
    (secret_key, public_key)
}

fn create_bridge_address(
    pubkeys: Vec<PublicKey>,
) -> Result<BitcoinAddress, Box<dyn std::error::Error>> {
    let secp = bitcoin::secp256k1::Secp256k1::new();

    let musig_key_agg_cache = KeyAggContext::new(pubkeys)?;

    let agg_pubkey = musig_key_agg_cache.aggregated_pubkey::<secp256k1_musig2::PublicKey>();
    let (xonly_agg_key, _) = agg_pubkey.x_only_public_key();

    // Convert to bitcoin XOnlyPublicKey first
    let internal_key = bitcoin::XOnlyPublicKey::from_slice(&xonly_agg_key.serialize())?;

    // Use internal_key for address creation
    let address = BitcoinAddress::p2tr(&secp, internal_key, None, Network::Regtest);

    Ok(address)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        return Err(
            "Usage: contributor [optional_public_key] OR coordinator public_key1 public_key2 ..."
                .into(),
        );
    }

    match args[1].as_str() {
        "contributor" => {
            let output = if args.len() > 2 {
                // Use provided public key
                let public_key = PublicKey::from_str(&args[2])?;
                ContributorOutput {
                    secret_key: None,
                    public_key,
                }
            } else {
                // Generate new keypair
                let (secret_key, public_key) = generate_keypair();
                ContributorOutput {
                    secret_key: Some(secret_key),
                    public_key,
                }
            };
            println!("{:?}", output);
            Ok(())
        }
        "coordinator" => {
            if args.len() <= 2 {
                return Err("Error: Coordinator needs at least one public key".into());
            }

            let mut pubkeys = Vec::new();
            for pubkey_str in args.iter().skip(2) {
                let public_key = PublicKey::from_str(pubkey_str).unwrap();
                pubkeys.push(public_key);
            }

            let bridge_address = create_bridge_address(pubkeys)?;
            let output = CoordinatorOutput {
                participant_count: args.len() - 2,
                bridge_address,
            };
            println!("{:?}", output);
            Ok(())
        }
        _ => Err("Invalid role. Use 'contributor' or 'coordinator'".into()),
    }
}
