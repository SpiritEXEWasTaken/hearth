//! `hearth-store`: the store repository's tool (run by its CI, and by maintainers).
//!
//! ```text
//! hearth-store lint [REPO]                       offline checks (schema, pins, duplicates, files…)
//! hearth-store combos [REPO] --out DIR           every option combination as compose + .env, for
//!                                                `docker compose config -q`
//! hearth-store images [REPO] --out images.json   registry checks: images exist for every listed
//!                                                architecture; records sizes for the badges
//! hearth-store build [REPO] --out dist [--images images.json] [--commit SHA]
//! hearth-store sign dist/index.json              key from $HEARTH_STORE_KEY (+ $HEARTH_STORE_KEY_PASSWORD)
//! hearth-store verify dist --pubkey KEY          check a built catalog like Hearth does
//! hearth-store keygen --out DIR --name NAME [--password]
//! hearth-store render [REPO] ID [--option k=v]   the merged compose file for those options
//! ```

use clap::{Parser, Subcommand};
use hearth_registry::{ImageRef, Platform, RegistryClient};
use hearth_store::catalog::{self, Bundle, ImageInfo, Source};
use hearth_store::lint::{self, HOST_VARS, images_of};
use hearth_store::model::OptionKind;
use hearth_store::resolve::{self, Hardware};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "hearth-store", about = "Hearth app store tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Lint {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Combos {
        #[arg(default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Images {
        #[arg(default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Only these entries (default: all).
        #[arg(long = "only")]
        only: Vec<String>,
    },
    Build {
        #[arg(default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        images: Option<PathBuf>,
        #[arg(long)]
        commit: Option<String>,
        #[arg(long, default_value = "https://github.com/SpiritEXEWasTaken/hearth-store")]
        source: String,
        /// Allow entries without screenshots (local test catalogs only; CI never uses this).
        #[arg(long)]
        draft: bool,
    },
    Sign {
        file: PathBuf,
    },
    Verify {
        dist: PathBuf,
        #[arg(long)]
        pubkey: Vec<String>,
    },
    Keygen {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        name: String,
        /// Ask for the secret key's password on the terminal (else $HEARTH_STORE_KEY_PASSWORD).
        #[arg(long)]
        password: bool,
    },
    Render {
        #[arg(default_value = ".")]
        repo: PathBuf,
        id: String,
        #[arg(long = "option", value_parser = parse_kv)]
        options: Vec<(String, String)>,
    },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| "use KEY=VALUE".to_string())
}

fn load(repo: &Path) -> Result<Vec<Source>, ExitCode> {
    catalog::load_repo(repo).map_err(|errs| {
        for e in errs {
            eprintln!("error: {e}");
        }
        ExitCode::FAILURE
    })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.cmd).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(c) => c,
    }
}

async fn run(cmd: Cmd) -> Result<(), ExitCode> {
    match cmd {
        Cmd::Lint { repo } => {
            let sources = load(&repo)?;
            let problems = lint::lint(&sources);
            for (id, p) in &problems {
                eprintln!("{id}: {p}");
            }
            if problems.is_empty() {
                println!("{} entries OK", sources.len());
                Ok(())
            } else {
                eprintln!("{} problem(s)", problems.len());
                Err(ExitCode::FAILURE)
            }
        }
        Cmd::Combos { repo, out } => {
            let sources = load(&repo)?;
            std::fs::create_dir_all(&out).map_err(io_fail)?;
            let hw = all_hardware();
            let mut n = 0;
            for s in &sources {
                for (i, combo) in resolve::combinations(&s.entry).into_iter().enumerate() {
                    let r = resolve::resolve(&s.entry, &combo, &hw);
                    let text =
                        resolve::render_compose(&s.compose, &s.variants, &r.variants).map_err(|e| {
                            eprintln!("{}: {e}", s.entry.id);
                            ExitCode::FAILURE
                        })?;
                    let dir = out.join(format!("{}--{i}", s.entry.id));
                    std::fs::create_dir_all(&dir).map_err(io_fail)?;
                    std::fs::write(dir.join("compose.yaml"), text).map_err(io_fail)?;
                    std::fs::write(dir.join(".env"), dummy_env(s, &r.env)).map_err(io_fail)?;
                    std::fs::write(dir.join("options.txt"), format!("{combo:?}\n")).map_err(io_fail)?;
                    n += 1;
                }
            }
            println!("{n} combinations written to {}", out.display());
            Ok(())
        }
        Cmd::Images { repo, out, only } => {
            let sources = load(&repo)?;
            let client = RegistryClient::new("hearth-store-ci").map_err(|e| {
                eprintln!("{e}");
                ExitCode::FAILURE
            })?;
            let mut result: BTreeMap<String, Vec<ImageInfo>> = BTreeMap::new();
            let mut failed = false;
            for s in sources
                .iter()
                .filter(|s| only.is_empty() || only.contains(&s.entry.id))
            {
                let (all, defaults) = entry_images(s);
                let mut infos = Vec::new();
                for img in all {
                    let r = match ImageRef::parse(&img) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("{}: {img}: {e}", s.entry.id);
                            failed = true;
                            continue;
                        }
                    };
                    let mut info = ImageInfo {
                        image: img.clone(),
                        platforms: Vec::new(),
                        size_mb: BTreeMap::new(),
                        default: defaults.contains(&img),
                    };
                    for arch in &s.entry.requirements.architectures {
                        let platform = Platform {
                            os: "linux".into(),
                            architecture: arch.clone(),
                        };
                        match client.describe(&r, &platform).await {
                            Ok(d) => {
                                let mut platforms = d.platforms.clone();
                                platforms.dedup();
                                platforms.sort();
                                platforms.dedup();
                                info.platforms = platforms;
                                let has = d.platforms.iter().any(|p| p == &format!("linux/{arch}"));
                                if !has && !info.default {
                                    // An optional choice (e.g. a CUDA build): Hearth checks it
                                    // against the server's CPU when installing.
                                    eprintln!(
                                        "{}: note: {img} (optional) has no linux/{arch} image",
                                        s.entry.id
                                    );
                                } else if !has {
                                    eprintln!(
                                        "{}: {img} has no linux/{arch} image (it has: {}); fix requirements.architectures",
                                        s.entry.id,
                                        d.platforms.join(", ")
                                    );
                                    failed = true;
                                } else if let Some(sz) = d.size {
                                    info.size_mb.insert(arch.clone(), sz.div_ceil(1024 * 1024));
                                }
                            }
                            Err(e) => {
                                eprintln!("{}: {img}: {e}", s.entry.id);
                                failed = true;
                            }
                        }
                    }
                    println!(
                        "{}: {img} [{}] {:?}",
                        s.entry.id,
                        info.platforms.join(" "),
                        info.size_mb
                    );
                    infos.push(info);
                }
                result.insert(s.entry.id.clone(), infos);
            }
            std::fs::write(&out, serde_json::to_vec_pretty(&result).unwrap_or_default()).map_err(io_fail)?;
            if failed { Err(ExitCode::FAILURE) } else { Ok(()) }
        }
        Cmd::Build {
            repo,
            out,
            images,
            commit,
            source,
            draft,
        } => {
            let sources = load(&repo)?;
            let mut problems = lint::lint(&sources);
            if draft {
                problems.retain(|(_, p)| p != "needs at least one screenshot");
            }
            if !problems.is_empty() {
                for (id, p) in &problems {
                    eprintln!("{id}: {p}");
                }
                return Err(ExitCode::FAILURE);
            }
            let imgs: BTreeMap<String, Vec<ImageInfo>> = match images {
                Some(p) => serde_json::from_slice(&std::fs::read(&p).map_err(io_fail)?).map_err(|e| {
                    eprintln!("{}: {e}", p.display());
                    ExitCode::FAILURE
                })?,
                None => BTreeMap::new(),
            };
            let index = catalog::build(&sources, &imgs, &out, now(), commit, &source).map_err(io_fail)?;
            let mut bundles: Vec<Bundle> = Vec::new();
            for a in &index.apps {
                let bytes = std::fs::read(out.join(&a.bundle)).map_err(io_fail)?;
                bundles.push(catalog::verify_bundle(&bytes, a).map_err(|e| {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                })?);
            }
            hearth_store::site::write(&out, &index, &bundles).map_err(io_fail)?;
            println!(
                "catalog with {} apps written to {}",
                index.apps.len(),
                out.display()
            );
            Ok(())
        }
        Cmd::Sign { file } => {
            let key_text = std::env::var("HEARTH_STORE_KEY").map_err(|_| {
                eprintln!("set HEARTH_STORE_KEY to the secret key file's contents");
                ExitCode::FAILURE
            })?;
            // Never prompt (CI has no terminal).
            let password = Some(std::env::var("HEARTH_STORE_KEY_PASSWORD").map_err(|_| {
                eprintln!("set HEARTH_STORE_KEY_PASSWORD to the key's password");
                ExitCode::FAILURE
            })?);
            let sk_box = minisign::SecretKeyBox::from_string(&key_text).map_err(sign_fail)?;
            let sk = sk_box.into_secret_key(password).map_err(sign_fail)?;
            let data = std::fs::read(&file).map_err(io_fail)?;
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("index.json");
            let trusted = format!("timestamp:{} file:{name}", now());
            let sig = minisign::sign(
                None,
                &sk,
                std::io::Cursor::new(&data),
                Some(&trusted),
                Some("hearth store"),
            )
            .map_err(sign_fail)?;
            let mut out = file.clone().into_os_string();
            out.push(".minisig");
            std::fs::write(&out, sig.to_string()).map_err(io_fail)?;
            println!("signed {}", file.display());
            Ok(())
        }
        Cmd::Verify { dist, pubkey } => {
            let index = std::fs::read(dist.join("index.json")).map_err(io_fail)?;
            let sig = std::fs::read_to_string(dist.join("index.json.minisig")).map_err(io_fail)?;
            let keys: Vec<&str> = if pubkey.is_empty() {
                hearth_store::TRUSTED_KEYS.to_vec()
            } else {
                pubkey.iter().map(String::as_str).collect()
            };
            let idx = catalog::verify_index(&index, &sig, &keys).map_err(|e| {
                eprintln!("{e}");
                ExitCode::FAILURE
            })?;
            for a in &idx.apps {
                let bytes = std::fs::read(dist.join(&a.bundle)).map_err(io_fail)?;
                let b = catalog::verify_bundle(&bytes, a).map_err(|e| {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                })?;
                let icon = std::fs::read(dist.join(&a.icon)).map_err(io_fail)?;
                if catalog::sha256_hex(&icon) != a.icon_sha256 {
                    eprintln!("{}: icon doesn't match", a.id);
                    return Err(ExitCode::FAILURE);
                }
                for (f, h) in &b.files {
                    let bytes = std::fs::read(dist.join("files").join(&a.id).join(f)).map_err(io_fail)?;
                    if &catalog::sha256_hex(&bytes) != h {
                        eprintln!("{}: {f} doesn't match", a.id);
                        return Err(ExitCode::FAILURE);
                    }
                }
            }
            println!("catalog OK: {} apps, signed", idx.apps.len());
            Ok(())
        }
        Cmd::Keygen { out, name, password } => {
            std::fs::create_dir_all(&out).map_err(io_fail)?;
            // Keys are always password-protected (from $HEARTH_STORE_KEY_PASSWORD, or asked).
            let pw = match std::env::var("HEARTH_STORE_KEY_PASSWORD") {
                Ok(p) if !p.is_empty() => p,
                _ if password => {
                    let a = prompt("Password for the secret key: ")?;
                    let b = prompt("Again: ")?;
                    if a != b || a.is_empty() {
                        eprintln!("the passwords differ or are empty");
                        return Err(ExitCode::FAILURE);
                    }
                    a
                }
                _ => {
                    eprintln!("set HEARTH_STORE_KEY_PASSWORD or pass --password");
                    return Err(ExitCode::FAILURE);
                }
            };
            let kp = minisign::KeyPair::generate_encrypted_keypair(Some(pw)).map_err(sign_fail)?;
            let sk = kp.sk.to_box(None).map_err(sign_fail)?.to_string();
            let pk = kp.pk.to_box().map_err(sign_fail)?.to_string();
            let sk_path = out.join(format!("{name}.key"));
            write_private(&sk_path, &sk)?;
            std::fs::write(out.join(format!("{name}.pub")), &pk).map_err(io_fail)?;
            println!("public key (for TRUSTED_KEYS): {}", kp.pk.to_base64());
            println!("secret key: {}", sk_path.display());
            Ok(())
        }
        Cmd::Render { repo, id, options } => {
            let sources = load(&repo)?;
            let Some(s) = sources.iter().find(|s| s.entry.id == id) else {
                eprintln!("no entry {id}");
                return Err(ExitCode::FAILURE);
            };
            let answers: BTreeMap<String, String> = options.into_iter().collect();
            let r = resolve::resolve(&s.entry, &answers, &all_hardware());
            for e in &r.errors {
                eprintln!("error: {e}");
            }
            let text = resolve::render_compose(&s.compose, &s.variants, &r.variants).map_err(|e| {
                eprintln!("{e}");
                ExitCode::FAILURE
            })?;
            println!(
                "# options: {:?}\n# variants: {:?}\n# env: {:?}\n{text}",
                r.answers, r.variants, r.env
            );
            Ok(())
        }
    }
}

fn all_hardware() -> Hardware {
    Hardware::everything()
}

/// Every image an entry can use, and the ones in its default install.
fn entry_images(s: &Source) -> (Vec<String>, BTreeSet<String>) {
    let mut all: Vec<String> = Vec::new();
    let mut defaults = BTreeSet::new();
    let none = Hardware::default();
    let default_r = resolve::resolve(&s.entry, &BTreeMap::new(), &none);
    if let Ok(t) = resolve::render_compose(&s.compose, &s.variants, &default_r.variants)
        && let Ok(doc) = serde_yaml_ng::from_str(&t)
    {
        defaults.extend(images_of(&doc).into_iter().map(|(_, i)| i));
    }
    for combo in resolve::combinations(&s.entry) {
        let r = resolve::resolve(&s.entry, &combo, &all_hardware());
        if let Ok(t) = resolve::render_compose(&s.compose, &s.variants, &r.variants)
            && let Ok(doc) = serde_yaml_ng::from_str(&t)
        {
            for (_, i) in images_of(&doc) {
                if !all.contains(&i) {
                    all.push(i);
                }
            }
        }
    }
    (all, defaults)
}

/// A `.env` that lets `docker compose config` check a combination.
fn dummy_env(s: &Source, env: &BTreeMap<String, String>) -> String {
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    for v in HOST_VARS {
        m.insert(
            v.to_string(),
            match *v {
                "PUID" | "PGID" => "1000".into(),
                "RENDER_GID" => "992".into(),
                "VIDEO_GID" => "44".into(),
                "TZ" => "Etc/UTC".into(),
                _ => "/tmp/hearth-check".into(),
            },
        );
    }
    for p in &s.entry.ports {
        if let Some(e) = &p.env {
            m.insert(e.clone(), p.port.to_string());
        }
    }
    for k in s.entry.env.keys() {
        m.insert(k.clone(), "x".into());
    }
    for o in &s.entry.options {
        if let (Some(e), OptionKind::Folder | OptionKind::Text) = (&o.env, o.kind) {
            m.insert(e.clone(), o.default.replace("${APP_DATA}", "/tmp/hearth-check"));
        }
    }
    for (k, v) in env {
        m.insert(k.clone(), v.replace("${APP_DATA}", "/tmp/hearth-check"));
    }
    m.into_iter().map(|(k, v)| format!("{k}={v}\n")).collect()
}

fn prompt(msg: &str) -> Result<String, ExitCode> {
    use std::io::{BufRead, Write};
    eprint!("{msg}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(io_fail)?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

fn write_private(path: &Path, text: &str) -> Result<(), ExitCode> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(io_fail)?;
    f.write_all(text.as_bytes()).map_err(io_fail)
}

fn io_fail(e: std::io::Error) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::FAILURE
}

fn sign_fail(e: minisign::PError) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::FAILURE
}
