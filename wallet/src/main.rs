// Copyright © 2021 Translucence Research, Inc. All rights reserved.

use crate::routes::{
    dispatch_url, dispatch_web_socket, CapeAPIError, RouteBinding, UrlSegmentType, UrlSegmentValue,
    Wallet,
};
use async_std::{
    sync::{Arc, Mutex},
    task::{spawn, JoinHandle},
};
use net::server;
use std::collections::hash_map::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use structopt::StructOpt;
use tide::StatusCode;
use tide_websockets::{WebSocket, WebSocketConnection};

mod disco;
mod ip;
mod mocks;
mod routes;
mod wallet;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "Wallet Web API",
    about = "Performs wallet operations in response to web requests"
)]
struct NodeOpt {
    /// Path to assets including web server files.
    #[structopt(
        long = "assets",
        default_value = ""      // See fn default_web_path().
    )]
    web_path: String,

    /// Path to API specification and messages.
    #[structopt(
        long = "api",
        default_value = ""      // See fn default_api_path().
    )]
    api_path: String,
}

/// Returns the project directory.
fn project_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Returns "<repo>/public/" where <repo> is
/// derived from the executable path assuming the executable is in
/// two directory levels down and the project directory name
/// can be derived from the executable name.
///
/// For example, if the executable path is
/// ```
///    ~/tri/systems/system/examples/multi_machine/target/release/multi_machine
/// ```
/// then the asset path is
/// ```
///    ~/tri/systems/system/examples/multi_machine/public/
/// ```
fn default_web_path() -> PathBuf {
    const ASSET_DIR: &str = "public";
    let dir = project_path();
    [&dir, Path::new(ASSET_DIR)].iter().collect()
}

/// Returns the default path to the API file.
fn default_api_path() -> PathBuf {
    const API_FILE: &str = "api/api.toml";
    let dir = project_path();
    [&dir, Path::new(API_FILE)].iter().collect()
}

#[derive(Clone)]
pub struct WebState {
    web_path: PathBuf,
    api: toml::Value,
    wallet: Arc<Mutex<Option<Wallet>>>,
}

async fn form_demonstration(req: tide::Request<WebState>) -> Result<tide::Body, tide::Error> {
    let mut index_html: PathBuf = req.state().web_path.clone();
    index_html.push("index.html");
    Ok(tide::Body::from_file(index_html).await?)
}

// Get the route pattern that matches the URL of a request, and the bindings for parameters in the
// pattern. If no route matches, the error is a documentation string explaining what went wrong.
fn parse_route(
    req: &tide::Request<WebState>,
) -> Result<(String, HashMap<String, RouteBinding>), String> {
    let first_segment = &req
        .url()
        .path_segments()
        .ok_or_else(|| String::from("No path segments"))?
        .next()
        .ok_or_else(|| String::from("Empty path"))?;
    let api = &req.state().api["route"][first_segment];
    let route_patterns = api["PATH"]
        .as_array()
        .expect("Invalid PATH type. Expecting array.");
    let mut arg_doc: String = api["DOC"].as_str().expect("Missing DOC").to_string();
    let mut matching_route_count = 0u64;
    let mut matching_route = String::new();
    let mut bindings: HashMap<String, HashMap<String, RouteBinding>> = HashMap::new();
    for route_pattern in route_patterns.iter() {
        let mut found_literal_mismatch = false;
        let mut argument_parse_failed = false;
        arg_doc.push_str(&format!(
            "\n\nRoute: {}\n--------------------\n",
            &route_pattern.as_str().unwrap()
        ));
        // The `path_segments()` succeeded above, so `unwrap()` is safe.
        let mut req_segments = req.url().path_segments().unwrap();
        for pat_segment in route_pattern
            .as_str()
            .expect("PATH must be an array of strings")
            .split('/')
        {
            // Each route parameter has an associated type. The lookup
            // will only succeed if the current segment is a parameter
            // placeholder, such as :id. Otherwise, it is assumed to
            // be a literal.
            if let Some(segment_type_value) = &api.get(pat_segment) {
                let segment_type = segment_type_value
                    .as_str()
                    .expect("The path pattern must be a string.");
                let req_segment = req_segments.next().unwrap_or("");
                arg_doc.push_str(&format!(
                    "  Argument: {} as type {} and value: {} ",
                    pat_segment, segment_type, req_segment
                ));
                let ptype =
                    UrlSegmentType::from_str(segment_type).map_err(|err| err.to_string())?;
                if let Some(value) = UrlSegmentValue::parse(ptype, req_segment) {
                    let rb = RouteBinding {
                        parameter: pat_segment.to_string(),
                        ptype,
                        value,
                    };
                    bindings
                        .entry(String::from(route_pattern.as_str().unwrap()))
                        .or_default()
                        .insert(pat_segment.to_string(), rb);
                    arg_doc.push_str("(Parse succeeded)\n");
                } else {
                    arg_doc.push_str("(Parse failed)\n");
                    argument_parse_failed = true;
                    // TODO !corbett capture parse failures documentation
                    // UrlSegmentValue::ParseFailed(segment_type, req_segment)
                }
            } else {
                // No type information. Assume pat_segment is a literal.
                let req_segment = req_segments.next().unwrap_or("");
                if req_segment != pat_segment {
                    found_literal_mismatch = true;
                    arg_doc.push_str(&format!(
                        "Request segment {} does not match route segment {}.\n",
                        req_segment, pat_segment
                    ));
                }
                // TODO !corbett else capture the matching literal in bindings
                // TODO !corebtt if the edit distance is small, capture spelling suggestion
            }
        }
        if !found_literal_mismatch {
            arg_doc.push_str(&format!(
                "Literals match for {}\n",
                &route_pattern.as_str().unwrap(),
            ));
        }
        let mut length_matches = false;
        if req_segments.next().is_none() {
            arg_doc.push_str(&format!(
                "Length match for {}\n",
                &route_pattern.as_str().unwrap(),
            ));
            length_matches = true;
        }
        if argument_parse_failed {
            arg_doc.push_str(&"Argument parsing failed.\n".to_string());
        } else {
            arg_doc.push_str(&"No argument parsing errors!\n".to_string());
        }
        if !argument_parse_failed && length_matches && !found_literal_mismatch {
            let route_pattern_str = route_pattern.as_str().unwrap();
            arg_doc.push_str(&format!("Route matches request: {}\n", &route_pattern_str));
            matching_route_count += 1;
            matching_route = String::from(route_pattern_str);
        } else {
            arg_doc.push_str("Route does not match request.\n");
        }
    }
    match matching_route_count {
        0 => {
            arg_doc.push_str("\nNeed documentation");
            Err(arg_doc)
        }
        1 => {
            let route_bindings = bindings.remove(&matching_route).unwrap_or_default();
            Ok((matching_route, route_bindings))
        }
        _ => {
            arg_doc.push_str("\nAmbiguity in api.toml");
            Err(arg_doc)
        }
    }
}

/// Handle API requests defined in api.toml.
///
/// This function duplicates the logic for deciding which route was requested. This
/// is an unfortunate side-effect of defining the routes in an external file.
// todo !corbett Convert the error feedback into HTML
async fn entry_page(req: tide::Request<WebState>) -> Result<tide::Response, tide::Error> {
    match parse_route(&req) {
        Ok((pattern, bindings)) => dispatch_url(req, pattern.as_str(), &bindings).await,
        Err(arg_doc) => Ok(tide::Response::builder(200).body(arg_doc).build()),
    }
}

async fn handle_web_socket(
    req: tide::Request<WebState>,
    wsc: WebSocketConnection,
) -> tide::Result<()> {
    match parse_route(&req) {
        Ok((pattern, bindings)) => dispatch_web_socket(req, wsc, pattern.as_str(), &bindings).await,
        Err(arg_doc) => Err(tide::Error::from_str(StatusCode::BadRequest, arg_doc)),
    }
}

// This route is a demonstration of a form with a WebSocket connection
// for asynchronous updates. Once we have useful forms, this can go...
fn add_form_demonstration(web_server: &mut tide::Server<WebState>) {
    web_server
        .at("/transfer/:id/:recipient/:amount")
        .with(WebSocket::new(handle_web_socket))
        .get(form_demonstration);
}

fn init_server(
    api_path: PathBuf,
    web_path: PathBuf,
    port: u64,
) -> std::io::Result<JoinHandle<std::io::Result<()>>> {
    let api = disco::load_messages(&api_path);
    let mut web_server = tide::with_state(WebState {
        web_path: web_path.clone(),
        api: api.clone(),
        wallet: Arc::new(Mutex::new(None)),
    });
    web_server
        .with(server::trace)
        .with(server::add_error_body::<_, CapeAPIError>);

    // Define the routes handled by the web server.
    web_server.at("/public").serve_dir(web_path)?;
    web_server.at("/").get(disco::compose_help);

    add_form_demonstration(&mut web_server);

    // Add routes from a configuration file.
    if let Some(api_map) = api["route"].as_table() {
        api_map.values().for_each(|v| {
            let web_socket = v
                .get("WEB_SOCKET")
                .map(|v| v.as_bool().expect("expected boolean value for WEB_SOCKET"))
                .unwrap_or(false);
            let routes = match &v["PATH"] {
                toml::Value::String(s) => {
                    vec![s.clone()]
                }
                toml::Value::Array(a) => a
                    .iter()
                    .filter_map(|v| {
                        if let Some(s) = v.as_str() {
                            Some(String::from(s))
                        } else {
                            println!("Oops! Array element: {:?}", v);
                            None
                        }
                    })
                    .collect(),
                _ => panic!("Expecting a toml::String or toml::Array, but got: {:?}", &v),
            };
            for path in routes {
                let mut route = web_server.at(&path);
                if web_socket {
                    route.get(WebSocket::new(handle_web_socket));
                } else {
                    route.get(entry_page);
                }
            }
        });
    }

    let addr = format!("0.0.0.0:{}", port);
    Ok(spawn(web_server.listen(addr)))
}

#[async_std::main]
async fn main() -> Result<(), std::io::Error> {
    tracing_subscriber::fmt().pretty().init();

    // Initialize the web server.
    //
    // opt_web_path is the path to the web assets directory. If the path
    // is empty, the default is constructed assuming Cargo is used to
    // build the executable in the customary location.
    //
    // own_id is the identifier of this instance of the executable. The
    // port the web server listens on is 60000, unless the
    // PORT environment variable is set.

    // Take the command line option for the web asset directory path
    // provided it is not empty. Otherwise, construct the default from
    // the executable path.
    let opt_api_path = NodeOpt::from_args().api_path;
    let opt_web_path = NodeOpt::from_args().web_path;
    let web_path = if opt_web_path.is_empty() {
        default_web_path()
    } else {
        PathBuf::from(opt_web_path)
    };
    let api_path = if opt_api_path.is_empty() {
        default_api_path()
    } else {
        PathBuf::from(opt_api_path)
    };
    println!("Web path: {:?}", web_path);

    // Use something different than the default Spectrum port (60000 vs 50000).
    let port = std::env::var("PORT").unwrap_or_else(|_| String::from("60000"));
    init_server(api_path, web_path, port.parse().unwrap())?.await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cap_rust_sandbox::state::{Erc20Code, EthereumAddr};
    use jf_aap::{
        keys::UserKeyPair,
        structs::{AssetCode, AssetDefinition},
    };
    use lazy_static::lazy_static;
    use net::{client, UserAddress};
    use rand_chacha::{rand_core::SeedableRng, ChaChaRng};
    use routes::{BalanceInfo, PubKey, WalletSummary};
    use seahorse::{hd::KeyTree, txn_builder::AssetInfo};
    use serde::de::DeserializeOwned;
    use std::convert::TryInto;
    use std::fmt::Debug;
    use std::iter::once;
    use surf::Url;
    use tagged_base64::TaggedBase64;
    use tempdir::TempDir;
    use tracing_test::traced_test;

    lazy_static! {
        static ref PORT: Arc<Mutex<u64>> = {
            let port_offset = std::env::var("PORT").unwrap_or_else(|_| String::from("60000"));
            Arc::new(Mutex::new(port_offset.parse().unwrap()))
        };
    }

    async fn port() -> u64 {
        let mut counter = PORT.lock().await;
        let port = *counter;
        *counter += 1;
        port
    }

    fn random_mnemonic(rng: &mut ChaChaRng) -> String {
        // TODO add an endpoint for generating random mnemonics
        KeyTree::random(rng).unwrap().1
    }

    struct TestServer {
        client: surf::Client,
        temp_dir: TempDir,
    }

    impl TestServer {
        async fn new() -> Self {
            let port = port().await;

            // Run a server in the background that is unique to this test. Note that the server task
            // is leaked: tide does not provide any mechanism for graceful programmatic shutdown, so
            // the server will continue running until the process is killed, even after the test
            // ends. This is probably not so bad, since each test's server task should be idle once
            // the test is over, and anyways I don't see a good way around it.
            init_server(default_api_path(), default_web_path(), port).unwrap();

            let client: surf::Client = surf::Config::new()
                .set_base_url(Url::parse(&format!("http://localhost:{}", port)).unwrap())
                .set_timeout(None)
                .try_into()
                .unwrap();
            Self {
                client: client.with(client::parse_error_body::<CapeAPIError>),
                temp_dir: TempDir::new("test_cape_wallet").unwrap(),
            }
        }

        async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, surf::Error> {
            let mut res = self.client.get(path).send().await?;
            client::response_body(&mut res).await
        }

        async fn requires_wallet<T: Debug + DeserializeOwned>(&self, path: &str) {
            self.get::<T>(path)
                .await
                .expect_err(&format!("{} succeeded without an open wallet", path));
        }

        fn path(&self) -> TaggedBase64 {
            TaggedBase64::new(
                "PATH",
                self.temp_dir
                    .path()
                    .as_os_str()
                    .to_str()
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap()
        }
    }

    #[async_std::test]
    #[traced_test]
    async fn test_newwallet() {
        let server = TestServer::new().await;
        let mut rng = ChaChaRng::from_seed([42u8; 32]);
        let mnemonic = random_mnemonic(&mut rng);

        // Should fail if the mnemonic is invalid.
        server
            .get::<()>(&format!(
                "newwallet/invalid-mnemonic/path/{}",
                server.path()
            ))
            .await
            .expect_err("newwallet succeeded with an invalid mnemonic");
        // Should fail if the path is invalid.
        server
            .get::<()>(&format!("newwallet/{}/path/invalid-path", mnemonic))
            .await
            .expect_err("newwallet succeeded with an invalid path");

        server
            .get::<()>(&format!("newwallet/{}/path/{}", mnemonic, server.path()))
            .await
            .unwrap();

        // Should fail if the wallet already exists.
        server
            .get::<()>(&format!("newwallet/{}/path/{}", mnemonic, server.path()))
            .await
            .expect_err("newwallet succeeded when a wallet already existed");
    }

    #[async_std::test]
    #[traced_test]
    async fn test_openwallet() {
        let server = TestServer::new().await;
        let mut rng = ChaChaRng::from_seed([42u8; 32]);
        let mnemonic = random_mnemonic(&mut rng);
        println!("mnemonic: {}", mnemonic);

        // Should fail if no wallet exists.
        server
            .requires_wallet::<()>(&format!("openwallet/{}/path/{}", mnemonic, server.path()))
            .await;

        // Now create a wallet so we can open it.
        server
            .get::<()>(&format!("newwallet/{}/path/{}", mnemonic, server.path()))
            .await
            .unwrap();
        server
            .get::<()>(&format!("openwallet/{}/path/{}", mnemonic, server.path()))
            .await
            .unwrap();

        // Should fail if the mnemonic is invalid.
        server
            .get::<()>(&format!(
                "openwallet/invalid-mnemonic/path/{}",
                server.path()
            ))
            .await
            .expect_err("openwallet succeeded with an invalid mnemonic");
        // Should fail if the mnemonic is incorrect.
        server
            .get::<()>(&format!(
                "openwallet/{}/path/{}",
                random_mnemonic(&mut rng),
                server.path()
            ))
            .await
            .expect_err("openwallet succeeded with the wrong mnemonic");
        // Should fail if the path is invalid.
        server
            .get::<()>(&format!("openwallet/{}/path/invalid-path", mnemonic))
            .await
            .expect_err("openwallet succeeded with an invalid path");
    }

    #[async_std::test]
    #[traced_test]
    async fn test_closewallet() {
        let server = TestServer::new().await;
        let mut rng = ChaChaRng::from_seed([42u8; 32]);

        // Should fail if a wallet is not already open.
        server.requires_wallet::<()>("closewallet").await;

        // Now open a wallet and close it.
        server
            .get::<()>(&format!(
                "newwallet/{}/path/{}",
                random_mnemonic(&mut rng),
                server.path()
            ))
            .await
            .unwrap();
        server.get::<()>("closewallet").await.unwrap();
    }

    #[async_std::test]
    #[traced_test]
    async fn test_getinfo() {
        let server = TestServer::new().await;
        let mut rng = ChaChaRng::from_seed([42u8; 32]);

        // Should fail if a wallet is not already open.
        server.requires_wallet::<WalletSummary>("getinfo").await;

        // Now open a wallet and call getinfo.
        server
            .get::<()>(&format!(
                "newwallet/{}/path/{}",
                random_mnemonic(&mut rng),
                server.path()
            ))
            .await
            .unwrap();
        let info = server.get::<WalletSummary>("getinfo").await.unwrap();

        // The info is not very interesting before we add any keys or assets, but that's for another
        // endpoint.
        assert_eq!(
            info,
            WalletSummary {
                addresses: vec![],
                spend_keys: vec![],
                audit_keys: vec![],
                freeze_keys: vec![],
                assets: vec![AssetInfo::from(AssetDefinition::native())]
            }
        )
    }

    #[async_std::test]
    #[traced_test]
    async fn test_getaddress() {
        let server = TestServer::new().await;
        let mut rng = ChaChaRng::from_seed([42u8; 32]);

        // Should fail if a wallet is not already open.
        server
            .requires_wallet::<Vec<UserAddress>>("getaddress")
            .await;

        // Now open a wallet and call getaddress.
        server
            .get::<()>(&format!(
                "newwallet/{}/path/{}",
                random_mnemonic(&mut rng),
                server.path()
            ))
            .await
            .unwrap();
        let addresses = server.get::<Vec<UserAddress>>("getaddress").await.unwrap();

        // The result is not very interesting before we add any keys, but that's for another
        // endpoint.
        assert_eq!(addresses, vec![]);
    }

    #[async_std::test]
    #[traced_test]
    async fn test_getbalance() {
        let server = TestServer::new().await;
        let mut rng = ChaChaRng::from_seed([42u8; 32]);

        let addr = UserAddress::from(UserKeyPair::generate(&mut rng).address());
        let asset = AssetCode::native();

        // Should fail if a wallet is not already open.
        server
            .requires_wallet::<BalanceInfo>("getbalance/all")
            .await;
        server
            .requires_wallet::<BalanceInfo>(&format!("getbalance/address/{}", addr))
            .await;
        server
            .requires_wallet::<BalanceInfo>(&format!("getbalance/address/{}/asset/{}", addr, asset))
            .await;

        // Now open a wallet.
        server
            .get::<()>(&format!(
                "newwallet/{}/path/{}",
                random_mnemonic(&mut rng),
                server.path()
            ))
            .await
            .unwrap();

        // We can now hit the endpoints successfully, although there are currently no balances
        // because we haven't added any keys or received any records.
        assert_eq!(
            server.get::<BalanceInfo>("getbalance/all").await.unwrap(),
            BalanceInfo::AllBalances(HashMap::default())
        );
        assert_eq!(
            server
                .get::<BalanceInfo>(&format!("getbalance/address/{}", addr))
                .await
                .unwrap(),
            // Even though this address has not been added to the wallet (and thus was not included
            // in the results of `getbalance/all`), if we specifically request its balance, the
            // wallet will check for records of each known asset type belonging to this address,
            // find none, and return a balance of 0 for that asset type. Since the wallet always
            // knows about the native asset type, this will actually return some data, rather than
            // an empty map or an error.
            BalanceInfo::AccountBalances(once((AssetCode::native(), 0)).collect())
        );
        assert_eq!(
            server
                .get::<BalanceInfo>(&format!("getbalance/address/{}/asset/{}", addr, asset))
                .await
                .unwrap(),
            BalanceInfo::Balance(0),
        );
        // If we query for a specific asset code, we should get a balance of 0 even if the wallet
        // doesn't know about this asset yet.
        assert_eq!(
            server
                .get::<BalanceInfo>(&format!(
                    "getbalance/address/{}/asset/{}",
                    addr,
                    AssetCode::random(&mut rng).0
                ))
                .await
                .unwrap(),
            BalanceInfo::Balance(0),
        );

        // Should fail with an invalid address (we'll get an invalid address by serializing an asset
        // code where the address should go.).
        server
            .get::<BalanceInfo>(&format!("getbalance/address/{}", asset))
            .await
            .expect_err("getbalance succeeded with an invalid address");
        server
            .get::<BalanceInfo>(&format!("getbalance/address/{}/asset/{}", asset, asset))
            .await
            .expect_err("getbalance succeeded with an invalid address");
        // Should fail with an invalid asset code (we'll use an address where the asset should go).
        server
            .get::<BalanceInfo>(&format!("getbalance/address/{}/asset/{}", addr, addr))
            .await
            .expect_err("getbalance succeeded with an invalid asset code");
        // Should fail with route pattern misuse (e.g. specifying `address` route component without
        // an accompanying `:address` parameter).
        server
            .get::<BalanceInfo>("getbalance/address")
            .await
            .expect_err("getbalance/address succeeded with invalid route pattern");
        server
            .get::<BalanceInfo>("getbalance/asset")
            .await
            .expect_err("getbalance/asset succeeded with invalid route pattern");
        server
            .get::<BalanceInfo>("getbalance")
            .await
            .expect_err("getbalance succeeded with invalid route pattern");
    }

    #[async_std::test]
    #[traced_test]
    async fn test_newkey() {
        let server = TestServer::new().await;
        let mut rng = ChaChaRng::from_seed([42u8; 32]);

        // Should fail if a wallet is not already open.
        server.requires_wallet::<PubKey>("newkey/send").await;
        server.requires_wallet::<PubKey>("newkey/trace").await;
        server.requires_wallet::<PubKey>("newkey/freeze").await;

        // Now open a wallet.
        server
            .get::<()>(&format!(
                "newwallet/{}/path/{}",
                random_mnemonic(&mut rng),
                server.path()
            ))
            .await
            .unwrap();

        // newkey should return a public key with the correct type and add the key to the wallet.
        let spend_key = server.get::<PubKey>("newkey/send").await.unwrap();
        let audit_key = server.get::<PubKey>("newkey/trace").await.unwrap();
        let freeze_key = server.get::<PubKey>("newkey/freeze").await.unwrap();
        let info = server.get::<WalletSummary>("getinfo").await.unwrap();
        match spend_key {
            PubKey::Spend(key) => {
                assert_eq!(info.spend_keys, vec![key]);
            }
            _ => {
                panic!("Expected PubKey::Spend, found {:?}", spend_key);
            }
        }
        match audit_key {
            PubKey::Audit(key) => {
                assert_eq!(info.audit_keys, vec![key]);
            }
            _ => {
                panic!("Expected PubKey::Audit, found {:?}", audit_key);
            }
        }
        match freeze_key {
            PubKey::Freeze(key) => {
                assert_eq!(info.freeze_keys, vec![key]);
            }
            _ => {
                panic!("Expected PubKey::Freeze, found {:?}", freeze_key);
            }
        }

        // Should fail if the key type is invaild.
        server
            .get::<PubKey>("newkey/invalid_key_type")
            .await
            .expect_err("newkey succeeded with an invaild key type");
    }

    #[async_std::test]
    #[traced_test]
    async fn test_newasset() {
        let server = TestServer::new().await;
        let mut rng = ChaChaRng::from_seed([42u8; 32]);

        // Set parameters for newasset.
        let erc20_code = Erc20Code(EthereumAddr([1u8; 20]));
        let sponsor_addr = EthereumAddr([2u8; 20]);
        let reveal_threshold = 10;
        let trace_amount = true;
        let trace_address = false;
        let description = TaggedBase64::new("DESC", &[3u8; 32]).unwrap();

        // Should fail if a wallet is not already open.
        server
            .requires_wallet::<AssetDefinition>(&format!(
                "newasset/erc20/{}/issuer/{}/traceamount/{}/traceaddress/{}/revealthreshold/{}",
                erc20_code, sponsor_addr, trace_amount, trace_address, reveal_threshold
            ))
            .await;
        server
            .requires_wallet::<AssetDefinition>(&format!(
                "newasset/description/{}/traceamount/{}/traceaddress/{}/revealthreshold/{}",
                description, trace_amount, trace_address, reveal_threshold
            ))
            .await;

        // Now open a wallet.
        server
            .get::<()>(&format!(
                "newwallet/{}/path/{}",
                random_mnemonic(&mut rng),
                server.path()
            ))
            .await
            .unwrap();

        // Create keys.
        server.get::<PubKey>("newkey/trace").await.unwrap();
        server.get::<PubKey>("newkey/freeze").await.unwrap();
        let info = server.get::<WalletSummary>("getinfo").await.unwrap();
        let audit_key = &info.audit_keys[0];
        let freeze_key = &info.freeze_keys[0];

        // newasset should return a sponsored asset with the correct policy if an ERC20 code is given.
        let sponsored_asset = server
            .get::<AssetDefinition>(&format!(
                "newasset/erc20/{}/issuer/{}/freezekey/{}/tracekey/{}/traceamount/{}/traceaddress/{}/revealthreshold/{}",
                erc20_code, sponsor_addr, freeze_key, audit_key, trace_amount, trace_address, reveal_threshold
            ))
            .await
            .unwrap();
        assert_eq!(sponsored_asset.policy_ref().auditor_pub_key(), audit_key);
        assert_eq!(sponsored_asset.policy_ref().freezer_pub_key(), freeze_key);
        assert_eq!(
            sponsored_asset.policy_ref().reveal_threshold(),
            reveal_threshold
        );

        // newasset should return a defined asset with the correct policy if no ERC20 code is given.
        let defined_asset = server
            .get::<AssetDefinition>(&format!(
                "newasset/description/{}/freezekey/{}/tracekey/{}/traceamount/{}/traceaddress/{}/revealthreshold/{}",
                description, freeze_key, audit_key, trace_amount, trace_address, reveal_threshold
            ))
            .await
            .unwrap();
        assert_eq!(defined_asset.policy_ref().auditor_pub_key(), audit_key);
        assert_eq!(defined_asset.policy_ref().freezer_pub_key(), freeze_key);
        assert_eq!(
            defined_asset.policy_ref().reveal_threshold(),
            reveal_threshold
        );
        let defined_asset = server
            .get::<AssetDefinition>(&format!(
            "newasset/freezekey/{}/tracekey/{}/traceamount/{}/traceaddress/{}/revealthreshold/{}",
            freeze_key, audit_key, trace_amount, trace_address, reveal_threshold
        ))
            .await
            .unwrap();
        assert_eq!(defined_asset.policy_ref().auditor_pub_key(), audit_key);
        assert_eq!(defined_asset.policy_ref().freezer_pub_key(), freeze_key);
        assert_eq!(
            defined_asset.policy_ref().reveal_threshold(),
            reveal_threshold
        );

        // newasset should return an asset with the default freezer public key if it's not given.
        let sponsored_asset = server
                .get::<AssetDefinition>(&format!(
                    "newasset/erc20/{}/issuer/{}/tracekey/{}/traceamount/{}/traceaddress/{}/revealthreshold/{}",
                    erc20_code, sponsor_addr, audit_key, trace_amount, trace_address, reveal_threshold
                ))
                .await
                .unwrap();
        assert!(!sponsored_asset.policy_ref().is_freezer_pub_key_set());
        let sponsored_asset = server
            .get::<AssetDefinition>(&format!(
                "newasset/description/{}/tracekey/{}/traceamount/{}/traceaddress/{}/revealthreshold/{}",
                description, audit_key, trace_amount, trace_address, reveal_threshold
            ))
            .await
            .unwrap();
        assert!(!sponsored_asset.policy_ref().is_freezer_pub_key_set());

        // newasset should return an asset with the default auditor public key and no reveal threshold if an
        // auditor public key isn't given.
        let sponsored_asset = server
            .get::<AssetDefinition>(&format!(
                "newasset/erc20/{}/issuer/{}/freezekey/{}",
                erc20_code, sponsor_addr, freeze_key
            ))
            .await
            .unwrap();
        assert!(!sponsored_asset.policy_ref().is_auditor_pub_key_set());
        assert_eq!(sponsored_asset.policy_ref().reveal_threshold(), 0);
        let sponsored_asset = server
            .get::<AssetDefinition>(&format!("newasset/description/{}", description))
            .await
            .unwrap();
        assert!(!sponsored_asset.policy_ref().is_auditor_pub_key_set());
        assert_eq!(sponsored_asset.policy_ref().reveal_threshold(), 0);

        // newasset should return an asset with no reveal threshold if it's not given.
        let sponsored_asset = server
                .get::<AssetDefinition>(&format!(
                    "newasset/erc20/{}/issuer/{}/freezekey/{}/tracekey/{}/traceamount/{}/traceaddress/{}",
                    erc20_code, sponsor_addr, freeze_key, audit_key, trace_amount, trace_address
                ))
                .await
                .unwrap();
        assert_eq!(sponsored_asset.policy_ref().reveal_threshold(), 0);
        let defined_asset = server
            .get::<AssetDefinition>(&format!(
                "newasset/description/{}/freezekey/{}/tracekey/{}/traceamount/{}/traceaddress/{}",
                description, freeze_key, audit_key, trace_amount, trace_address
            ))
            .await
            .unwrap();
        assert_eq!(defined_asset.policy_ref().reveal_threshold(), 0);
    }
}
