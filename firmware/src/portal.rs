//! Captive setup portal: open AP + DNS that answers everything with our IP + one web page.
//! HTTP handlers only touch `Shared`; the main loop does the actual Wi-Fi work.
use crate::net::Network;
use anyhow::Result;
use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::{Read, Write};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiState {
    Connecting(String),
    Connected(String),
    Failed(String),
}

#[derive(Default)]
pub struct Shared {
    pub networks: Vec<Network>,
    pub page_opened: bool,
    /// The phone's OS hit a connectivity-check URL.
    pub probed: bool,
    /// Submitted by the phone, taken by the main loop.
    pub wifi_request: Option<(String, String)>,
    pub wifi: Option<WifiState>,
    pub city_request: Option<usize>,
}

pub struct Portal {
    _http: EspHttpServer<'static>,
    stop: Arc<AtomicBool>,
    pub shared: Arc<Mutex<Shared>>,
}

impl Drop for Portal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push(' '),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// application/x-www-form-urlencoded value decoding.
fn form_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form_get(body: &str, key: &str) -> Option<String> {
    body.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| form_decode(v))
    })
}

fn page(shared: &Shared) -> String {
    let mut nets = String::new();
    for n in &shared.networks {
        nets.push_str(&format!(
            "<option value=\"{0}\">{0}{1}</option>",
            esc(&n.ssid),
            if n.open { " (open)" } else { "" }
        ));
    }
    let mut cities = String::new();
    for (i, c) in ezan::cities::CITIES.iter().enumerate() {
        cities.push_str(&format!(
            "<option value=\"{i}\"{}>{} ({})</option>",
            if i == ezan::cities::DEFAULT_CITY { " selected" } else { "" },
            esc(c.name),
            c.utc_label
        ));
    }
    PAGE.replace("{{NETWORKS}}", &nets).replace("{{CITIES}}", &cities)
}

const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>AdhanBox setup</title>
<style>
:root{--fg:#111;--bg:#f4f2ee;--mut:#666;--line:#d8d4cc}
*{box-sizing:border-box}body{margin:0;font:17px/1.4 -apple-system,system-ui,sans-serif;background:var(--bg);color:var(--fg)}
main{max-width:420px;margin:0 auto;padding:28px 18px}
h1{font-size:24px;margin:0 0 4px}p.sub{color:var(--mut);margin:0 0 24px}
.step{font-size:13px;letter-spacing:.06em;text-transform:uppercase;color:var(--mut);margin:0 0 8px}
label{display:block;font-weight:600;margin:16px 0 6px}
select,input{width:100%;font:inherit;padding:12px;border:1px solid var(--line);border-radius:10px;background:#fff;color:var(--fg)}
button{width:100%;margin-top:22px;padding:14px;font:inherit;font-weight:600;border:0;border-radius:10px;background:var(--fg);color:var(--bg)}
button:disabled{opacity:.5}
.msg{margin-top:16px;padding:12px;border-radius:10px;background:#fff;border:1px solid var(--line)}
.err{border-color:#c33;color:#a11}.ok{border-color:#2a7;color:#175}
.hide{display:none}.hint{color:var(--mut);font-size:14px;margin-top:8px}
</style></head><body><main>
<h1>AdhanBox</h1><p class="sub">Prayer times &amp; adhan</p>

<section id="s2">
<p class="step">Step 2 of 5 · Wi-Fi</p>
<form id="wf">
<label for="ssid">Network</label>
<select id="ssid" name="ssid">{{NETWORKS}}<option value="__other">Other…</option></select>
<input id="other" class="hide" placeholder="Network name" style="margin-top:8px">
<label for="pass">Password</label>
<input id="pass" name="pass" type="password" autocomplete="current-password">
<p class="hint">2.4 GHz networks only.</p>
<button id="cb" type="submit">Connect</button>
</form>
<div id="wmsg" class="msg hide"></div>
</section>

<section id="s3" class="hide">
<p class="step">Step 3 of 5 · City</p>
<form id="cf">
<label for="city">City</label>
<select id="city" name="city">{{CITIES}}</select>
<button type="submit">Save</button>
</form>
</section>

<section id="s4" class="hide"><div class="msg ok">All set. You can disconnect your phone; AdhanBox will finish setting up on its own.</div></section>

<script>
const $=id=>document.getElementById(id);
$('ssid').onchange=()=>$('other').classList.toggle('hide',$('ssid').value!=='__other');
function show(id){['s2','s3','s4'].forEach(s=>$(s).classList.toggle('hide',s!==id))}
function msg(t,cls){const m=$('wmsg');m.textContent=t;m.className='msg '+(cls||'');}
$('wf').onsubmit=async e=>{e.preventDefault();
 const ssid=$('ssid').value==='__other'?$('other').value:$('ssid').value;
 $('cb').disabled=true;msg('Connecting to '+ssid+'…');
 await fetch('/wifi',{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:'ssid='+encodeURIComponent(ssid)+'&pass='+encodeURIComponent($('pass').value)});
 poll();};
async function poll(){for(let i=0;i<60;i++){await new Promise(r=>setTimeout(r,1000));
 let s;try{s=await (await fetch('/status')).json()}catch(e){continue}
 if(s.state==='connected'){msg('Connected ✓','ok');setTimeout(()=>show('s3'),700);return}
 if(s.state==='failed'){msg(s.detail,'err');$('pass').value='';$('cb').disabled=false;return}}
 msg('No answer from AdhanBox. Try again.','err');$('cb').disabled=false;}
$('cf').onsubmit=async e=>{e.preventDefault();
 await fetch('/city',{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded'},body:'city='+$('city').value});
 show('s4');};
</script></main></body></html>"#;

fn read_body(req: &mut impl Read) -> String {
    let mut buf = [0u8; 512];
    let mut out = Vec::new();
    while let Ok(n) = req.read(&mut buf) {
        if n == 0 || out.len() > 2048 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn start(networks: Vec<Network>, ip: [u8; 4]) -> Result<Portal> {
    let home = format!("http://{}.{}.{}.{}/", ip[0], ip[1], ip[2], ip[3]);
    let shared = Arc::new(Mutex::new(Shared { networks, ..Default::default() }));
    let stop = Arc::new(AtomicBool::new(false));

    // DNS: answer every A query with our IP so phones show the captive sheet.
    let dns_stop = stop.clone();
    std::thread::Builder::new().stack_size(4096).spawn(move || {
        let sock = match UdpSocket::bind("0.0.0.0:53") {
            Ok(s) => s,
            Err(e) => {
                log::error!("dns: bind failed: {e}");
                return;
            }
        };
        log::info!("dns: captive resolver up");
        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(500)));
        let mut buf = [0u8; 512];
        while !dns_stop.load(Ordering::SeqCst) {
            let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
            if n < 12 {
                continue;
            }
            // Find end of the question (QNAME + QTYPE + QCLASS)
            let mut i = 12;
            while i < n && buf[i] != 0 {
                i += buf[i] as usize + 1;
            }
            let qend = i + 5;
            if qend > n {
                continue;
            }
            let mut r = Vec::with_capacity(qend + 16);
            r.extend_from_slice(&buf[..2]); // id
            r.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
            r.extend_from_slice(&buf[12..qend]);
            r.extend_from_slice(&[0xC0, 0x0C, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, ip[0], ip[1], ip[2], ip[3]]);
            let _ = sock.send_to(&r, from);
            log::debug!("dns: answered {from}");
        }
    })?;

    let mut http = EspHttpServer::new(&Configuration {
        uri_match_wildcard: true,
        max_uri_handlers: 12,
        stack_size: 8192,
        ..Default::default()
    })?;

    let s = shared.clone();
    http.fn_handler("/", Method::Get, move |req| -> Result<(), esp_idf_svc::io::EspIOError> {
        log::info!("portal: page opened");
        let html = {
            let mut g = s.lock().unwrap();
            g.page_opened = true;
            page(&g)
        };
        req.into_response(200, None, &[("Content-Type", "text/html; charset=utf-8"), ("Cache-Control", "no-store")])?
            .write_all(html.as_bytes())?;
        Ok(())
    })?;

    let s = shared.clone();
    http.fn_handler("/wifi", Method::Post, move |mut req| -> Result<(), esp_idf_svc::io::EspIOError> {
        let body = read_body(&mut req);
        let ssid = form_get(&body, "ssid").unwrap_or_default();
        let pass = form_get(&body, "pass").unwrap_or_default();
        {
            let mut g = s.lock().unwrap();
            g.wifi = Some(WifiState::Connecting(ssid.clone()));
            g.wifi_request = Some((ssid, pass));
        }
        req.into_ok_response()?.write_all(b"ok")?;
        Ok(())
    })?;

    let s = shared.clone();
    http.fn_handler("/status", Method::Get, move |req| -> Result<(), esp_idf_svc::io::EspIOError> {
        let j = match s.lock().unwrap().wifi.clone() {
            None => r#"{"state":"idle"}"#.to_string(),
            Some(WifiState::Connecting(n)) => format!(r#"{{"state":"connecting","detail":{}}}"#, json_str(&n)),
            Some(WifiState::Connected(n)) => format!(r#"{{"state":"connected","detail":{}}}"#, json_str(&n)),
            Some(WifiState::Failed(m)) => format!(r#"{{"state":"failed","detail":{}}}"#, json_str(&m)),
        };
        req.into_response(200, None, &[("Content-Type", "application/json"), ("Cache-Control", "no-store")])?
            .write_all(j.as_bytes())?;
        Ok(())
    })?;

    let s = shared.clone();
    http.fn_handler("/city", Method::Post, move |mut req| -> Result<(), esp_idf_svc::io::EspIOError> {
        let body = read_body(&mut req);
        if let Some(i) = form_get(&body, "city").and_then(|v| v.parse::<usize>().ok()) {
            if i < ezan::cities::CITIES.len() {
                s.lock().unwrap().city_request = Some(i);
            }
        }
        req.into_ok_response()?.write_all(b"ok")?;
        Ok(())
    })?;

    // Everything else (captive-portal probes like /generate_204, /hotspot-detect.html) → our page.
    let s = shared.clone();
    http.fn_handler("/*", Method::Get, move |req| -> Result<(), esp_idf_svc::io::EspIOError> {
        log::info!("portal: probe {} -> redirect", req.uri());
        s.lock().unwrap().probed = true;
        req.into_response(302, None, &[("Location", home.as_str())])?.write_all(b"")?;
        Ok(())
    })?;

    Ok(Portal { _http: http, stop, shared })
}
