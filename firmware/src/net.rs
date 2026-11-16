//! Wi-Fi (station, setup AP), SNTP and the Diyanet fetch.
use anyhow::{bail, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::http::client::{Configuration as HttpConf, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::io::Read;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi,
};
use ezan::DayTimes;
use std::time::{Duration, Instant};

pub const DIYANET_BASE: &str = "https://ezanvakti.emushaf.net";
pub const USER_AGENT: &str = "AdhanBox/0.1 (+https://github.com/azer/adhanbox)";

#[derive(Clone, Debug)]
pub struct Network {
    pub ssid: String,
    pub rssi: i8,
    pub open: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    NotFound,
    WrongPassword,
    Timeout,
    NoInternet,
}

impl ConnectError {
    pub fn message(self) -> &'static str {
        match self {
            ConnectError::NotFound => "Network not found",
            ConnectError::WrongPassword => "Wrong password, try again",
            ConnectError::Timeout => "Couldn't connect, try again",
            ConnectError::NoInternet => "Connected, but no internet",
        }
    }
}

pub struct Net {
    wifi: BlockingWifi<EspWifi<'static>>,
    pub ap_ssid: String,
}

impl Net {
    pub fn new(modem: Modem<'static>, sysloop: EspSystemEventLoop, nvs: EspDefaultNvsPartition) -> Result<Self> {
        let esp = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;
        let mac = esp.sta_netif().get_mac().unwrap_or([0; 6]);
        let ap_ssid = format!("AdhanBox-{:02X}{:02X}", mac[4], mac[5]);
        let wifi = BlockingWifi::wrap(esp, sysloop)?;
        Ok(Self { wifi, ap_ssid })
    }

    pub fn is_connected(&self) -> bool {
        self.wifi.is_connected().unwrap_or(false)
    }

    fn client(ssid: &str, pass: &str) -> ClientConfiguration {
        ClientConfiguration {
            ssid: ssid.try_into().unwrap_or_default(),
            password: pass.try_into().unwrap_or_default(),
            auth_method: if pass.is_empty() { AuthMethod::None } else { AuthMethod::WPA2Personal },
            ..Default::default()
        }
    }

    /// Scan in station mode. Strongest first, de-duplicated, 2.4 GHz only (the radio has no 5 GHz).
    pub fn scan(&mut self) -> Vec<Network> {
        let _ = self.wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()));
        if !self.wifi.is_started().unwrap_or(false) {
            let _ = self.wifi.start();
        }
        let mut out: Vec<Network> = Vec::new();
        if let Ok(list) = self.wifi.scan() {
            for ap in list {
                let ssid = ap.ssid.as_str().to_string();
                if ssid.is_empty() || out.iter().any(|n| n.ssid == ssid) {
                    continue;
                }
                out.push(Network { ssid, rssi: ap.signal_strength, open: ap.auth_method == Some(AuthMethod::None) });
            }
        }
        out.sort_by(|a, b| b.rssi.cmp(&a.rssi));
        out
    }

    /// Start the open setup AP (plus a station interface for trying credentials).
    pub fn start_setup_ap(&mut self) -> Result<()> {
        let ap = AccessPointConfiguration {
            ssid: self.ap_ssid.as_str().try_into().unwrap_or_default(),
            auth_method: AuthMethod::None,
            channel: 6,
            max_connections: 4,
            ..Default::default()
        };
        let _ = self.wifi.stop();
        self.wifi.set_configuration(&Configuration::Mixed(ClientConfiguration::default(), ap))?;
        self.wifi.start()?;
        self.captive_dhcp();
        Ok(())
    }

    /// The setup AP's IPv4 address (esp-idf-svc default is 192.168.71.1).
    pub fn ap_ip(&self) -> [u8; 4] {
        self.wifi.wifi().ap_netif().get_ip_info().map(|i| i.ip.octets()).unwrap_or([192, 168, 71, 1])
    }

    /// Make the AP's DHCP server hand out our IP as DNS (so the captive DNS answers the phone's
    /// connectivity probe) and advertise the setup page via DHCP option 114 (RFC 8910, iOS 14+).
    fn captive_dhcp(&mut self) {
        use esp_idf_svc::sys::*;
        let netif = self.wifi.wifi().ap_netif().handle();
        unsafe {
            let mut ip: esp_netif_ip_info_t = core::mem::zeroed();
            esp_netif_get_ip_info(netif, &mut ip);
            esp_netif_dhcps_stop(netif);
            let mut offer: u8 = 0x02; // OFFER_DNS
            let r1 = esp_netif_dhcps_option(
                netif,
                esp_netif_dhcp_option_mode_t_ESP_NETIF_OP_SET,
                esp_netif_dhcp_option_id_t_ESP_NETIF_DOMAIN_NAME_SERVER,
                &mut offer as *mut u8 as *mut _,
                1,
            );
            let mut dns: esp_netif_dns_info_t = core::mem::zeroed();
            dns.ip.u_addr.ip4.addr = ip.ip.addr;
            dns.ip.type_ = 0; // IPADDR_TYPE_V4
            let r2 = esp_netif_set_dns_info(netif, esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN, &mut dns);
            let o = ip.ip.addr.to_le_bytes();
            let mut uri = format!("http://{}.{}.{}.{}/\0", o[0], o[1], o[2], o[3]).into_bytes();
            let r3 = esp_netif_dhcps_option(
                netif,
                esp_netif_dhcp_option_mode_t_ESP_NETIF_OP_SET,
                esp_netif_dhcp_option_id_t_ESP_NETIF_CAPTIVEPORTAL_URI,
                uri.as_mut_ptr() as *mut _,
                (uri.len() - 1) as u32,
            );
            let r4 = esp_netif_dhcps_start(netif);
            log::info!("captive dhcp: dns={r1}/{r2} uri={r3} start={r4}");
        }
    }

    pub fn ap_station_count(&self) -> usize {
        let mut list: esp_idf_svc::sys::wifi_sta_list_t = unsafe { core::mem::zeroed() };
        if unsafe { esp_idf_svc::sys::esp_wifi_ap_get_sta_list(&mut list) } == 0 {
            list.num as usize
        } else {
            0
        }
    }

    /// Try credentials while keeping the setup AP up.
    pub fn try_connect_keep_ap(&mut self, ssid: &str, pass: &str) -> Result<(), ConnectError> {
        let ap = match self.wifi.get_configuration() {
            Ok(Configuration::Mixed(_, ap)) => ap,
            _ => return Err(ConnectError::Timeout),
        };
        let _ = self.wifi.wifi_mut().disconnect();
        self.wifi
            .set_configuration(&Configuration::Mixed(Self::client(ssid, pass), ap))
            .map_err(|_| ConnectError::Timeout)?;
        self.connect_inner(ssid)
    }

    /// Station-only connect (daily sync).
    pub fn connect(&mut self, ssid: &str, pass: &str) -> Result<(), ConnectError> {
        let _ = self.wifi.stop();
        self.wifi
            .set_configuration(&Configuration::Client(Self::client(ssid, pass)))
            .map_err(|_| ConnectError::Timeout)?;
        self.wifi.start().map_err(|_| ConnectError::Timeout)?;
        self.connect_inner(ssid)
    }

    fn connect_inner(&mut self, ssid: &str) -> Result<(), ConnectError> {
        let start = Instant::now();
        let mut last_reason = 0u32;
        while start.elapsed() < Duration::from_secs(20) {
            match self.wifi.wifi_mut().connect() {
                Ok(()) => {}
                Err(e) => log::warn!("connect call: {e:?}"),
            }
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(8) {
                if self.wifi.is_connected().unwrap_or(false) {
                    if self.wifi.wait_netif_up().is_ok() {
                        return Ok(());
                    }
                    return Err(ConnectError::NoInternet);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            last_reason += 1;
        }
        let _ = last_reason;
        // Distinguish "not found" from "wrong password" with a quick scan.
        let visible = self
            .wifi
            .scan()
            .map(|l| l.iter().any(|ap| ap.ssid.as_str() == ssid))
            .unwrap_or(true);
        Err(if visible { ConnectError::WrongPassword } else { ConnectError::NotFound })
    }

    pub fn stop(&mut self) {
        let _ = self.wifi.stop();
    }
}

/// Blocking SNTP sync (≤ 15 s). Returns UTC unix time.
pub fn sntp_sync() -> Result<i64> {
    let sntp = EspSntp::new_default()?;
    let start = Instant::now();
    while sntp.get_sync_status() != SyncStatus::Completed {
        if start.elapsed() > Duration::from_secs(15) {
            bail!("SNTP timeout");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(crate::clock::now())
}

/// HTTPS GET into memory.
pub fn get(url: &str) -> Result<Vec<u8>> {
    let mut conn = EspHttpConnection::new(&HttpConf {
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(Duration::from_secs(20)),
        buffer_size: Some(2048),
        ..Default::default()
    })?;
    conn.initiate_request(Method::Get, url, &[("User-Agent", USER_AGENT), ("Accept", "application/json")])?;
    conn.initiate_response()?;
    let status = conn.status();
    if status != 200 {
        bail!("HTTP {status} from {url}");
    }
    let mut body = Vec::with_capacity(24 * 1024);
    let mut buf = [0u8; 1024];
    loop {
        let n = conn.read(&mut buf).context("reading body")?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > 256 * 1024 {
            bail!("response too large");
        }
    }
    Ok(body)
}

/// Monthly Diyanet times for a district.
pub fn fetch_times(diyanet_id: u32) -> Result<Vec<DayTimes>> {
    let body = get(&format!("{DIYANET_BASE}/vakitler/{diyanet_id}"))?;
    Ok(ezan::parse::parse_vakitler(&body)?)
}
