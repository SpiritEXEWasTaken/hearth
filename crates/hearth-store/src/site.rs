//! The public app browser on the store's GitHub Pages site: a static page per entry plus a
//! searchable list. No tracking, no external requests: everything is served from the same site.

use crate::catalog::{Bundle, Index, IndexApp};
use crate::model::{GpuUse, HttpsNeed};
use std::path::Path;

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn size_text(mb: u64) -> String {
    if mb >= 1024 {
        let gb = mb as f64 / 1024.0;
        if gb >= 10.0 {
            format!("{gb:.0} GB")
        } else {
            format!("{gb:.1} GB")
        }
    } else {
        format!("{mb} MB")
    }
}

/// Short badge texts, in the same words as Hearth's store page.
pub fn badges(a: &IndexApp) -> Vec<(&'static str, String)> {
    let b = &a.badges;
    let mut v = vec![("ram", format!("RAM {}", size_text(b.ram_mb)))];
    let dl = b
        .download_mb
        .get("amd64")
        .or_else(|| b.download_mb.values().next())
        .copied()
        .unwrap_or(0);
    v.push(("disk", format!("Disk ~{}", size_text(dl + b.data_mb))));
    let arch = if b.architectures.len() == 1 {
        match b.architectures[0].as_str() {
            "amd64" => "x86 (Intel/AMD) only".to_string(),
            "arm64" => "ARM only".to_string(),
            o => o.to_string(),
        }
    } else {
        "x86 + ARM".to_string()
    };
    v.push(("cpu", arch));
    match b.gpu {
        GpuUse::None => {}
        GpuUse::Optional => v.push(("gpu", "Can use a GPU".into())),
        GpuUse::Recommended => v.push(("gpu", "GPU recommended".into())),
        GpuUse::Required => v.push(("gpu", "Needs a GPU".into())),
    }
    match b.https {
        HttpsNeed::No => {}
        HttpsNeed::Recommended => v.push(("https", "HTTPS recommended".into())),
        HttpsNeed::Required => v.push(("https", "Needs HTTPS".into())),
    }
    if b.host_network {
        v.push(("net", "Host network".into()));
    }
    v
}

const CSS: &str = r#"
:root{--bg:#faf8f5;--fg:#1d1b19;--muted:#6b645c;--card:#fff;--line:#e7e1d9;--accent:#c2410c;--chip:#f3ede6}
@media(prefers-color-scheme:dark){:root{--bg:#161412;--fg:#eee9e3;--muted:#a59d94;--card:#201d1a;--line:#36312c;--accent:#fb923c;--chip:#2b2723}}
*{box-sizing:border-box}body{margin:0;font:16px/1.5 system-ui,sans-serif;background:var(--bg);color:var(--fg)}
a{color:var(--accent)}header{padding:24px 20px 8px;max-width:1100px;margin:auto}
header h1{margin:0;font-size:26px}header p{margin:4px 0 0;color:var(--muted)}
main{max-width:1100px;margin:auto;padding:12px 20px 40px}
.bar{display:flex;flex-wrap:wrap;gap:8px;margin:8px 0 16px}
.bar input{flex:1 1 240px;padding:10px 12px;border:1px solid var(--line);border-radius:10px;background:var(--card);color:var(--fg);font-size:16px}
.chip{border:1px solid var(--line);background:var(--chip);color:var(--fg);padding:6px 12px;border-radius:999px;cursor:pointer;font-size:14px}
.chip[aria-pressed=true]{background:var(--accent);color:#fff;border-color:var(--accent)}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:14px}
.card{display:flex;gap:12px;padding:14px;border:1px solid var(--line);border-radius:14px;background:var(--card);text-decoration:none;color:inherit}
.card:hover{border-color:var(--accent)}.card img{width:48px;height:48px;object-fit:contain;flex:none}
.card h3{margin:0;font-size:17px}.card p{margin:2px 0 6px;color:var(--muted);font-size:14px}
.badges{display:flex;flex-wrap:wrap;gap:4px}.b{font-size:12px;padding:2px 8px;border-radius:999px;background:var(--chip);color:var(--muted)}
.b.https,.b.gpu{color:var(--accent)}
.shots{display:flex;gap:12px;overflow-x:auto;scroll-snap-type:x mandatory;padding-bottom:8px}
.shots figure{margin:0;flex:0 0 min(90%,640px);scroll-snap-align:start}
.shots img{width:100%;border:1px solid var(--line);border-radius:10px}.shots figcaption{font-size:13px;color:var(--muted)}
.notice{border-left:4px solid var(--accent);background:var(--chip);padding:10px 14px;border-radius:6px;margin:14px 0}
.top{display:flex;gap:16px;align-items:center}.top img{width:72px;height:72px}
dl{display:grid;grid-template-columns:max-content 1fr;gap:4px 16px}dt{color:var(--muted)}dd{margin:0}
footer{max-width:1100px;margin:auto;padding:20px;color:var(--muted);font-size:14px}
"#;

const JS: &str = r#"
const q=document.getElementById('q'),cards=[...document.querySelectorAll('.card')],chips=[...document.querySelectorAll('.chip')];let cat='';
function f(){const t=q.value.trim().toLowerCase();for(const c of cards){const ok=(!cat||c.dataset.cat===cat)&&(!t||c.dataset.text.includes(t));c.style.display=ok?'':'none'}}
q.addEventListener('input',f);for(const ch of chips)ch.addEventListener('click',()=>{cat=ch.dataset.cat;for(const x of chips)x.setAttribute('aria-pressed',x===ch);f()});
"#;

fn page(title: &str, root: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>{}</title><link rel=stylesheet href=\"{root}style.css\"></head><body>{body}\
<footer>Hearth app store · entries are checked by CI and the catalog is signed · \
<a href=\"https://github.com/SpiritEXEWasTaken/hearth-store\">source</a> · \
<a href=\"https://github.com/SpiritEXEWasTaken/hearth\">Hearth</a></footer></body></html>",
        esc(title)
    )
}

fn badge_html(a: &IndexApp) -> String {
    badges(a)
        .into_iter()
        .map(|(k, t)| format!("<span class=\"b {k}\">{}</span>", esc(&t)))
        .collect()
}

/// Write `index.html`, `app/<id>/index.html`, `style.css` and `store.js` into `out` (which
/// already holds the catalog).
pub fn write(out: &Path, index: &Index, bundles: &[Bundle]) -> std::io::Result<()> {
    std::fs::write(out.join("style.css"), CSS)?;
    std::fs::write(out.join("store.js"), JS)?;
    std::fs::write(out.join(".nojekyll"), "")?;
    let mut cards = String::new();
    for a in &index.apps {
        let text = format!("{} {} {} {}", a.name, a.summary, a.tags.join(" "), a.category).to_lowercase();
        cards.push_str(&format!(
            "<a class=card href=\"app/{id}/\" data-cat=\"{cat}\" data-text=\"{text}\"><img src=\"{icon}\" alt=\"\">\
<div><h3>{name}</h3><p>{summary}</p><div class=badges>{badges}</div></div></a>",
            id = esc(&a.id),
            cat = esc(&a.category),
            text = esc(&text),
            icon = esc(&a.icon),
            name = esc(&a.name),
            summary = esc(&a.summary),
            badges = badge_html(a),
        ));
    }
    let mut chips = String::from("<button class=chip data-cat=\"\" aria-pressed=true>All</button>");
    for c in &index.categories {
        chips.push_str(&format!(
            "<button class=chip data-cat=\"{}\" aria-pressed=false>{}</button>",
            esc(&c.id),
            esc(&c.name)
        ));
    }
    let body = format!(
        "<header><h1>Hearth app store</h1><p>{n} apps for your home server. Install them from your Hearth: \
<b>Store</b> in the menu.</p></header><main><div class=bar><input id=q type=search placeholder=\"Search apps\" \
aria-label=\"Search apps\"></div><div class=bar>{chips}</div><div class=grid>{cards}</div></main>\
<script src=\"store.js\"></script>",
        n = index.apps.len()
    );
    std::fs::write(out.join("index.html"), page("Hearth app store", "", &body))?;

    for b in bundles {
        let e = &b.entry;
        let Some(a) = index.apps.iter().find(|a| a.id == e.id) else {
            continue;
        };
        let dir = out.join("app").join(&e.id);
        std::fs::create_dir_all(&dir)?;
        let mut shots = String::new();
        for s in &e.screenshots {
            shots.push_str(&format!(
                "<figure><img loading=lazy src=\"../../files/{}/{}\" alt=\"{}\"><figcaption>{}</figcaption></figure>",
                esc(&e.id),
                esc(&s.file),
                esc(&s.caption),
                esc(&s.caption)
            ));
        }
        let desc: String = e
            .description
            .split("\n\n")
            .map(|p| format!("<p>{}</p>", esc(p.trim())))
            .collect();
        let notice = e
            .notice
            .as_ref()
            .map(|n| format!("<div class=notice>{}</div>", esc(n)))
            .unwrap_or_default();
        let mut opts = String::new();
        for o in &e.options {
            let what = if o.choices.is_empty() {
                o.help.clone().unwrap_or_default()
            } else {
                o.choices
                    .iter()
                    .map(|c| c.label.clone())
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            opts.push_str(&format!("<dt>{}</dt><dd>{}</dd>", esc(&o.label), esc(&what)));
        }
        let opts = if opts.is_empty() {
            String::new()
        } else {
            format!("<h2>Install options</h2><dl>{opts}</dl>")
        };
        let ports: String = e
            .ports
            .iter()
            .map(|p| {
                format!(
                    "<dt>{}</dt><dd>{}/{}</dd>",
                    esc(&p.label),
                    p.port,
                    p.protocol.as_str()
                )
            })
            .collect();
        let shots = if shots.is_empty() {
            String::new()
        } else {
            format!("<div class=shots>{shots}</div>")
        };
        let body = format!(
            "<header><a href=\"../../\">← All apps</a></header><main><div class=top><img src=\"../../{icon}\" alt=\"\">\
<div><h1>{name}</h1><p>{summary}</p><div class=badges>{badges}</div></div></div>{notice}{shots}{desc}\
<p>Install it from your Hearth: <b>Store → {name}</b>.</p>{opts}<h2>Details</h2><dl>\
<dt>Category</dt><dd>{cat}</dd><dt>Website</dt><dd><a href=\"{web}\">{web}</a></dd>\
<dt>Source</dt><dd><a href=\"{src}\">{src}</a></dd><dt>Licence</dt><dd>{lic}</dd>{ports}</dl></main>",
            icon = esc(&a.icon),
            name = esc(&e.name),
            summary = esc(&e.summary),
            badges = badge_html(a),
            cat = esc(e.category_name()),
            web = esc(&e.upstream.website),
            src = esc(&e.upstream.source),
            lic = esc(&e.license),
        );
        std::fs::write(
            dir.join("index.html"),
            page(&format!("{} · Hearth app store", e.name), "../../", &body),
        )?;
    }
    Ok(())
}
