use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

const SSID: &str = env!("ESP_SSID");
const PASSWORD: &str = env!("ESP_PASSWORD");

// 物理的に分離された「APサーバー」のIPアドレスとポート
const AP_SERVER_ADDRESS: &str = "192.168.2.105:8080";

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("[Web Tier] 専用フロントエンド Proxy サーバーを起動中...");

    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // ✨ 通信成立時に光らせるためのオンボードLED（GPIO2）の初期化
    let mut led = PinDriver::output(peripherals.pins.gpio2)?;
    led.set_low()?;

    // Wi-Fi接続処理
    let mut wifi = EspWifi::new(peripherals.modem, sys_loop, Some(nvs))?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: SSID.try_into().unwrap(),
        password: PASSWORD.try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.connect()?;

    // IPアドレスが確定するまで待機
    loop {
        if wifi.is_connected()? {
            let ip_info = wifi.sta_netif().get_ip_info()?;
            if ip_info.ip != std::net::Ipv4Addr::new(0, 0, 0, 0) {
                log::info!("[Web Tier] インフラ開通！ IPアドレス: {}", ip_info.ip);
                break;
            }
        }
        thread::sleep(Duration::from_millis(500));
    }

    let listener = TcpListener::bind("0.0.0.0:8085")?;
    log::info!("[Web Tier] 外界からのHTTPリクエストをポート 80 で待機中...");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let peer_addr = stream.peer_addr()?.to_string();
                log::info!("[Web Tier] ブラウザ {} からのアクセスを検知！", peer_addr);

                let mut buffer = [0; 1024];
                let _ = stream.read(&mut buffer)?;

                log::info!(
                    "[Web Tier] ➡️ APサーバー ({}) へ処理を中継します...",
                    AP_SERVER_ADDRESS
                );

                let mut ap_report = String::new();
                match TcpStream::connect(AP_SERVER_ADDRESS) {
                    Ok(mut ap_stream) => {
                        let command = format!("FORWARD_REQUEST:{}\n", peer_addr);
                        ap_stream.write_all(command.as_bytes())?;
                        ap_stream.flush()?;

                        let mut ap_buffer = [0; 2048];
                        if let Ok(bytes_read) = ap_stream.read(&mut ap_buffer) {
                            ap_report =
                                String::from_utf8_lossy(&ap_buffer[..bytes_read]).to_string();
                        }

                        // ✨ 通信・処理成立の物理インジケーターON
                        led.set_high()?;
                    }
                    Err(e) => {
                        log::error!("[Web Tier] ❌ APサーバーへの接続に失敗しました: {}", e);
                        ap_report = format!(
                            "<p style='color:red;'>[Web Error] AP Tier Cluster Down: {}</p>",
                            e
                        );
                    }
                }

                // 🌐 デザインを1ミリも崩さず、埋め込み文字列の無駄なスペースだけを極限まで詰めて軽量化
                let response_body = format!(
                    "<!DOCTYPE html><html><head><meta charset='utf-8'><title>Distributed 3-Tier Cluster</title>\
                    <style>body{{font-family:monospace;background:#0f172a;color:#38bdf8;padding:30px;}}\
                    .box{{border:2px solid #0284c7;background:#1e293b;padding:20px;border-radius:12px;box-shadow:0 4px 6px -1px rgba(0,0,0,0.5);}}\
                    h1{{color:#bae6fd;border-bottom:2px solid #0284c7;padding-bottom:10px;}}li{{color:#34d399;}}</style></head>\
                    <body><div class='box'><h1>🌐 [Web Tier] Edge Proxy Node Active</h1>\
                    <p><b>Client Peer IP:</b> {}</p>{}</div></body></html>",
                    peer_addr,
                    ap_report
                );

                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/html; charset=UTF-8\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                    response_body.len(),
                    response_body
                );

                stream.write_all(response.as_bytes())?;
                stream.flush()?;

                thread::sleep(Duration::from_millis(150));
                led.set_low()?;
            }
            Err(e) => log::error!("[Web Tier] 接続エラー: {}", e),
        }
    }
    Ok(())
}
