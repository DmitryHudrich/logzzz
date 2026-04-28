use super::state::PAGE_SIZE;
use super::types::CredRecord;

pub fn sanitize_filename(query: &str, search_type: &str, page: usize) -> String {
    let safe: String = query
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    format!("logzz_{}_{}__p{:03}.html", search_type, safe, page + 1)
}

fn safe_name_for_links(query: &str) -> String {
    query
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn render_html_report(
    records: &[CredRecord],
    query: &str,
    search_type: &str,
    page: usize,
    has_next: bool,
    total_unique: u64,
) -> String {
    let search_label = if search_type == "login" {
        "Login"
    } else {
        "URL"
    };
    let total_paths: usize = records.iter().map(|r| r.all_paths.len()).sum();
    let first = page * PAGE_SIZE + 1;
    let last = first + records.len() - 1;
    let safe = safe_name_for_links(query);

    let pagination_note = if total_unique > PAGE_SIZE as u64 {
        format!(" · records {first}–{last} of {total_unique}")
    } else {
        String::new()
    };

    let has_prev = page > 0;
    let nav_html = if has_prev || has_next {
        let prev_btn = if has_prev {
            format!(
                r#"<a class="nav-btn" href="logzz_{stype}_{safe}__p{prev:03}.html">◀ Page {prev}</a>"#,
                stype = search_type,
                safe = safe,
                prev = page,
            )
        } else {
            String::new()
        };
        let next_btn = if has_next {
            format!(
                r#"<a class="nav-btn" href="logzz_{stype}_{safe}__p{next:03}.html">Page {next} ▶</a>"#,
                stype = search_type,
                safe = safe,
                next = page + 2,
            )
        } else {
            String::new()
        };
        format!(r#"<div class="nav-bar">{}{}</div>"#, prev_btn, next_btn)
    } else {
        String::new()
    };

    let rows_html: String = records
        .iter()
        .enumerate()
        .map(|(i, r)| render_record(i, r))
        .collect();
    let now = time::UtcDateTime::now().to_string();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1.0"/>
<title>Logzz — {search_label}: {query_esc} (p.{page_n})</title>
<style>
  @import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;500;700&family=Syne:wght@400;700;800&display=swap');
  *, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}
  :root {{
    --bg:#0a0b0f; --surface:#111318; --surface2:#13151d;
    --border:#1e2130; --border-hi:#2e3450;
    --accent:#4f7cff; --accent2:#00e5b0; --danger:#ff4f6a; --warn:#ffb84f;
    --text:#c8cedf; --text-dim:#525970; --text-hi:#eef0f8;
    --mono:'JetBrains Mono',monospace; --display:'Syne',sans-serif; --r:12px;
  }}
  body {{ background:var(--bg); color:var(--text); font-family:var(--mono); font-size:13px; line-height:1.6; min-height:100vh; }}
  body::before {{
    content:''; position:fixed; inset:0; z-index:0; pointer-events:none;
    background-image:url("data:image/svg+xml,%3Csvg viewBox='0 0 256 256' xmlns='http://www.w3.org/2000/svg'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.9' numOctaves='4' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)' opacity='0.03'/%3E%3C/svg%3E");
  }}
  .shell {{ position:relative; z-index:1; max-width:980px; margin:0 auto; padding:48px 24px 80px; }}
  header {{ margin-bottom:40px; padding-bottom:28px; border-bottom:1px solid var(--border); }}
  .logo-row {{ display:flex; align-items:center; gap:12px; margin-bottom:16px; }}
  .logo-mark {{
    width:36px; height:36px; background:var(--accent); border-radius:8px;
    display:grid; place-items:center; font-family:var(--display); font-weight:800;
    font-size:17px; color:#fff; letter-spacing:-1px; box-shadow:0 0 24px rgba(79,124,255,.35);
  }}
  .logo-name {{ font-family:var(--display); font-weight:800; font-size:20px; color:var(--text-hi); letter-spacing:-.5px; }}
  h1 {{ font-family:var(--display); font-weight:800; font-size:clamp(24px,4vw,38px); color:var(--text-hi); letter-spacing:-1.5px; line-height:1.1; margin-bottom:14px; }}
  h1 .hl {{ color:var(--accent); }}
  .meta-row {{ display:flex; flex-wrap:wrap; gap:10px; }}
  .chip {{ display:inline-flex; align-items:center; gap:5px; padding:3px 11px; border:1px solid var(--border-hi); border-radius:99px; font-size:10.5px; color:var(--text-dim); letter-spacing:.4px; text-transform:uppercase; }}
  .chip.a1 {{ border-color:var(--accent);  color:var(--accent);  font-weight:700; }}
  .chip.a2 {{ border-color:var(--accent2); color:var(--accent2); font-weight:700; }}
  .chip.pg {{ border-color:var(--warn);    color:var(--warn);    font-weight:700; }}
  .nav-bar {{ display:flex; justify-content:space-between; align-items:center; margin-bottom:28px; gap:12px; }}
  .nav-btn {{ display:inline-flex; align-items:center; gap:6px; padding:8px 20px; border-radius:8px; background:var(--surface); border:1px solid var(--border-hi); color:var(--accent); font-family:var(--mono); font-size:12px; text-decoration:none; transition:background .15s,border-color .15s; }}
  .nav-btn:hover {{ background:rgba(79,124,255,.08); border-color:var(--accent); }}
  .records {{ display:flex; flex-direction:column; gap:14px; }}
  .record {{ background:var(--surface); border:1px solid var(--border); border-radius:var(--r); overflow:hidden; opacity:0; transform:translateY(8px); animation:fadeUp .36s ease forwards; transition:border-color .15s,box-shadow .15s; }}
  .record:hover {{ border-color:var(--border-hi); box-shadow:0 4px 32px rgba(0,0,0,.45); }}
  @keyframes fadeUp {{ to {{ opacity:1; transform:none; }} }}
  .record-top {{ display:flex; }}
  .record-index {{ width:52px; min-width:52px; display:grid; place-items:center; background:rgba(255,255,255,.018); border-right:1px solid var(--border); color:var(--text-dim); font-size:10px; font-weight:500; letter-spacing:.5px; writing-mode:vertical-rl; text-orientation:mixed; transform:rotate(180deg); user-select:none; }}
  .record-body {{ flex:1; display:flex; flex-direction:column; padding:15px 18px; gap:9px; }}
  .field {{ display:flex; align-items:baseline; gap:10px; flex-wrap:wrap; }}
  .label {{ font-size:9px; font-weight:700; letter-spacing:1.4px; text-transform:uppercase; color:var(--text-dim); min-width:76px; flex-shrink:0; padding-top:2px; }}
  .value {{ color:var(--text-hi); word-break:break-all; }}
  .url-val  {{ color:var(--accent2); }}
  .pass-val {{ color:var(--danger); font-weight:500; }}
  .src-val  {{ color:var(--text-dim); font-size:11.5px; }}
  .extra-tag {{ font-size:9px; font-weight:700; letter-spacing:.5px; text-transform:uppercase; padding:1px 7px; border:1px solid var(--border-hi); border-radius:99px; color:var(--text-dim); }}
  .src-field {{ padding-top:8px; border-top:1px solid var(--border); margin-top:2px; }}
  .drawer {{ border-top:1px solid var(--border); background:var(--surface2); }}
  .drawer-toggle {{ width:100%; display:flex; align-items:center; gap:10px; padding:9px 18px; background:none; border:none; cursor:pointer; color:var(--text-dim); font-family:var(--mono); font-size:11px; text-align:left; transition:color .15s,background .15s; }}
  .drawer-toggle:hover {{ color:var(--warn); background:rgba(255,184,79,.04); }}
  .arrow {{ display:inline-block; transition:transform .2s ease; font-style:normal; line-height:1; }}
  .dup-pill {{ background:rgba(255,184,79,.12); color:var(--warn); border:1px solid rgba(255,184,79,.28); border-radius:99px; padding:1px 8px; font-size:10px; font-weight:700; }}
  .one-pill {{ background:rgba(82,89,112,.12); color:var(--text-dim); border:1px solid var(--border); border-radius:99px; padding:1px 8px; font-size:10px; }}
  .drawer-body {{ display:none; flex-direction:column; gap:0; padding:2px 18px 14px 18px; }}
  .drawer[data-open="true"] .drawer-body {{ display:flex; }}
  .drawer[data-open="true"] .arrow {{ transform:rotate(90deg); }}
  .path-row {{ display:flex; align-items:flex-start; gap:8px; padding:5px 0; border-bottom:1px solid rgba(255,255,255,.03); font-size:11.5px; }}
  .path-row:last-child {{ border-bottom:none; }}
  .path-n   {{ color:var(--text-dim); font-size:10px; min-width:22px; flex-shrink:0; padding-top:1px; }}
  .path-str {{ color:var(--text-dim); word-break:break-all; transition:color .12s; }}
  .path-row:hover .path-str {{ color:var(--text); }}
  .footer-nav {{ margin-top:48px; padding-top:24px; border-top:1px solid var(--border); display:flex; justify-content:space-between; align-items:center; flex-wrap:wrap; gap:12px; }}
  .footer-meta {{ color:var(--text-dim); font-size:11px; }}
  @media (max-width:600px) {{ .record-index {{ display:none; }} .label {{ min-width:60px; }} }}
</style>
</head>
<body>
<div class="shell">
  <header>
    <div class="logo-row">
      <div class="logo-mark">Lz</div>
      <span class="logo-name">Logzz</span>
    </div>
    <h1>Results for <span class="hl">{query_esc}</span></h1>
    <div class="meta-row">
      <span class="chip a1">⬡ {total_unique} unique</span>
      <span class="chip a2">⊞ {total_paths} occurrences</span>
      <span class="chip pg">p. {page_n}{pagination_note}</span>
      <span class="chip">⊙ {search_label}</span>
      <span class="chip">⏱ {now}</span>
    </div>
  </header>
  {nav_html}
  <div class="records">
{rows_html}
  </div>
  <div class="footer-nav">
    {nav_html}
    <span class="footer-meta">Generated by Logzz · {now}</span>
  </div>
</div>
<script>
  document.querySelectorAll('.drawer-toggle').forEach(btn => {{
    btn.addEventListener('click', () => {{
      const d = btn.closest('.drawer');
      d.dataset.open = d.dataset.open === 'true' ? 'false' : 'true';
    }});
  }});
</script>
</body>
</html>"#,
        search_label = search_label,
        query_esc = html_escape(query),
        page_n = page + 1,
        total_unique = total_unique,
        total_paths = total_paths,
        pagination_note = pagination_note,
        now = now,
        rows_html = rows_html,
        nav_html = nav_html,
    )
}

fn render_record(idx: usize, r: &CredRecord) -> String {
    let extra_tag = if r.extra_json.len() > 2 {
        r#" <span class="extra-tag">+extra</span>"#
    } else {
        ""
    };
    let path_count = r.all_paths.len();
    let has_dups = path_count > 1;

    let paths_html: String = r
        .all_paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            format!(
                r#"      <div class="path-row"><span class="path-n">{n}.</span><span class="path-str">{path}</span></div>"#,
                n = i + 1,
                path = html_escape(p),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let pill = if has_dups {
        format!(r#"<span class="dup-pill">{} files</span>"#, path_count)
    } else {
        r#"<span class="one-pill">1 file</span>"#.to_string()
    };
    let drawer_label = if has_dups {
        format!("{} — expand to see all source paths", pill)
    } else {
        format!("{} — expand to see source path", pill)
    };

    format!(
        r#"<div class="record" style="animation-delay:{delay}ms">
  <div class="record-top">
    <div class="record-index">#{num:04}</div>
    <div class="record-body">
      <div class="field"><span class="label">URL</span><span class="value url-val">{url}</span></div>
      <div class="field"><span class="label">USERNAME</span><span class="value">{username}</span></div>
      <div class="field"><span class="label">PASSWORD</span><span class="value pass-val">{password}</span></div>
      <div class="field src-field">
        <span class="label">SOURCE</span>
        <span class="value src-val">{primary}</span>{extra}
      </div>
    </div>
  </div>
  <div class="drawer" data-open="false">
    <button class="drawer-toggle"><span class="arrow">▶</span>{drawer_label}</button>
    <div class="drawer-body">
{paths_html}
    </div>
  </div>
</div>"#,
        delay = (idx % PAGE_SIZE) * 28,
        num = idx + 1,
        url = html_escape(&r.url),
        username = html_escape(&r.username),
        password = html_escape(&r.password),
        primary = html_escape(&r.primary_path),
        extra = extra_tag,
        drawer_label = drawer_label,
        paths_html = paths_html,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
