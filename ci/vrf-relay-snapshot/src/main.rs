use {
    sha2::{Digest, Sha256},
    std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
        process::Command,
    },
    syn::{Attribute, Expr, ImplItem, Item, Lit, Meta},
};

fn private(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") { return false; }
        let Meta::List(list) = &attr.meta else { return false; };
        let Ok(Meta::NameValue(value)) = syn::parse2::<Meta>(list.tokens.clone()) else { return false; };
        value.path.is_ident("feature") && matches!(&value.value, Expr::Lit(value) if matches!(&value.lit, Lit::Str(value) if value.value()=="local-prover"))
    })
}

fn attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(v) => &v.attrs,
        Item::Enum(v) => &v.attrs,
        Item::ExternCrate(v) => &v.attrs,
        Item::Fn(v) => &v.attrs,
        Item::ForeignMod(v) => &v.attrs,
        Item::Impl(v) => &v.attrs,
        Item::Macro(v) => &v.attrs,
        Item::Mod(v) => &v.attrs,
        Item::Static(v) => &v.attrs,
        Item::Struct(v) => &v.attrs,
        Item::Trait(v) => &v.attrs,
        Item::TraitAlias(v) => &v.attrs,
        Item::Type(v) => &v.attrs,
        Item::Union(v) => &v.attrs,
        Item::Use(v) => &v.attrs,
        _ => &[],
    }
}

fn prune(items: &mut Vec<Item>) {
    items.retain(|item| {
        !private(attrs(item)) && !matches!(item, Item::Mod(module) if module.ident=="development")
    });
    for item in items {
        match item {
            Item::Impl(value) => value.items.retain(|item| {
                !private(match item {
                    ImplItem::Const(v) => &v.attrs,
                    ImplItem::Fn(v) => &v.attrs,
                    ImplItem::Type(v) => &v.attrs,
                    ImplItem::Macro(v) => &v.attrs,
                    _ => &[],
                })
            }),
            Item::Mod(value) => {
                if let Some((_, items)) = &mut value.content {
                    prune(items);
                }
            }
            _ => {}
        }
    }
}

fn source(root: &Path, revision: &str, name: &str) -> Vec<u8> {
    let output = Command::new("git")
        .args([
            "-C",
            root.to_str().expect("source path"),
            "show",
            &format!("{revision}:{name}"),
        ])
        .output()
        .expect("read source object");
    assert!(output.status.success(), "reviewed source object missing");
    output.stdout
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(
        args.len(),
        3,
        "usage: source-checkout reviewed-revision native-root"
    );
    let source_root = PathBuf::from(&args[0]);
    let revision = &args[1];
    let native = PathBuf::from(&args[2]);
    assert!(
        revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()),
        "full reviewed revision required"
    );
    let output = native.join("northstar/vrf-relay");
    let protocol = native.join("northstar/vrf-interface");
    fs::create_dir_all(output.join("src")).unwrap();
    fs::create_dir_all(protocol.join("src")).unwrap();
    fs::create_dir_all(protocol.join("tests")).unwrap();
    let files = [
        "accounts",
        "credentials",
        "crypto",
        "discovery",
        "instructions",
        "journal",
        "keys",
        "lib",
        "portal",
        "protocol",
        "remote_prover",
        "request_signer",
        "rpc",
        "serde_keys",
        "service",
        "settings",
    ];
    let mut origin = BTreeMap::new();
    let mut paths = Vec::new();
    for module in files {
        let name = format!("src/{module}.rs");
        let input = source(&source_root, revision, &name);
        origin.insert(name, hex::encode(Sha256::digest(&input)));
        let mut file = syn::parse_file(std::str::from_utf8(&input).unwrap())
            .expect("valid reviewed Rust source");
        prune(&mut file.items);
        let path = output.join(format!("src/{module}.rs"));
        fs::write(&path, prettyplease::unparse(&file)).unwrap();
        paths.push((path, "2024"));
    }
    let manifest = source(&source_root, revision, "relay/Cargo.toml");
    origin.insert(
        "relay/Cargo.toml".into(),
        hex::encode(Sha256::digest(&manifest)),
    );
    let manifest = String::from_utf8(manifest)
        .unwrap()
        .replace("path = \"../src/lib.rs\"", "path = \"src/lib.rs\"")
        .replace("path = \"../protocol\"", "path = \"../vrf-interface\"")
        .replace("zeroize = \"=1.8.2\"", "zeroize = \"1.8.2\"");
    fs::write(output.join("Cargo.toml"), manifest).unwrap();
    for (upstream, local) in [
        ("protocol/Cargo.toml", "Cargo.toml"),
        ("protocol/src/lib.rs", "src/lib.rs"),
        ("protocol/tests/membership.rs", "tests/membership.rs"),
    ] {
        let input = source(&source_root, revision, upstream);
        origin.insert(upstream.into(), hex::encode(Sha256::digest(&input)));
        let path = protocol.join(local);
        fs::write(&path, input).unwrap();
        if local.ends_with(".rs") {
            paths.push((path, "2021"));
        }
    }
    for (path, edition) in paths {
        assert!(
            Command::new("rustup")
                .args(["run", "nightly", "rustfmt", "--edition", edition])
                .arg(path)
                .status()
                .unwrap()
                .success(),
            "snapshot formatting failed"
        );
    }
    let mut published = BTreeMap::new();
    for root in [&output, &protocol] {
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|x| x == "rs")
                    || path.file_name().is_some_and(|x| x == "Cargo.toml")
                {
                    published.insert(
                        path.strip_prefix(native.join("northstar"))
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                        hex::encode(Sha256::digest(fs::read(path).unwrap())),
                    );
                }
            }
        }
    }
    let text = fs::read_to_string(output.join("src/crypto.rs")).unwrap();
    assert!(
        !text.contains("pub struct OracleKey")
            && !text.contains("EdVrfEdwards25519TaiSecretKey")
            && !text.contains("fn from_seed"),
        "private prover must not be published"
    );
    let text = fs::read_to_string(output.join("src/request_signer.rs")).unwrap();
    assert!(
        !text.contains("struct RequestSigner")
            && !text.contains("struct SignerPolicy")
            && !text.contains("struct SignerConfig"),
        "private signer must not be published"
    );
    fs::write(output.join("source-lock.json"),serde_json::to_vec_pretty(&serde_json::json!({"version":1,"repository":"https://github.com/mirrorworld-universe/vrf-oracle","revision":revision,"scope":"authorized relay-only source snapshot; signer/prover implementations and deployment state excluded","transformation":"remove local-prover Rust items and standalone development module; adapt package paths and compatible zeroize constraint","upstream_sha256":origin,"published_sha256":published})).unwrap()).unwrap();
    println!("relay-only snapshot generated; private signer and prover excluded");
}
