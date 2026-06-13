use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::PinDriver;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};
use std::io::{BufRead, BufReader, Read, Write}; // ✨ Read をしっかり追加しました！
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

// 物理的に分離された「APサーバー（2台目）」のIPアドレスとポート
const AP_SERVER_ADDRESS: &str = "192.168.2.103:8080";

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("[Web Tier] 専用フロントエンド Proxy サーバーを起動中...");

    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    //  通信成立時に光らせるためのオンボードLED（GPIO2）の初期化
    let mut led = PinDriver::output(peripherals.pins.gpio2)?;
    led.set_low()?;

    // Wi-Fi接続処理
    let mut wifi = EspWifi::new(peripherals.modem, sys_loop, Some(nvs))?;

    // 環境変数からSSIDとPASSを安全に固定長文字列（heapless）へ格納
    let mut ssid = heapless::String::<32>::new();
    let _ = ssid.push_str(env!("ESP_SSID"));

    let mut password = heapless::String::<64>::new();
    let _ = password.push_str(env!("ESP_PASSWORD"));

    log::info!("[Web Tier] Wi-Fi 接続を試みています: {}...", ssid);

    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid,
        password,
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

    // ポート 8085 でブラウザからのリクエストを待機
    let listener = TcpListener::bind("0.0.0.0:8085")?;
    log::info!("[Web Tier] 外界からのHTTPリクエストをポート 8085 で待機中...");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let peer_addr = stream.peer_addr()?.to_string();

                // BufReaderを使い、HTTPヘッダーの最初の1行（Request Line）だけを瞬時に読み込む
                let mut reader = BufReader::new(&mut stream);
                let mut request_line = String::new();
                if let Err(_) = reader.read_line(&mut request_line) {
                    continue;
                }

                if request_line.is_empty() {
                    continue;
                }

                // JavaScriptからの非同期ログ要求（/api/stream）の判定
                if request_line.starts_with("GET /api/stream")
                    || request_line.contains("/api/stream")
                {
                    log::info!("[Web Tier]  JavaScriptからの要求を検知。2台目へ中継...");

                    let mut ap_report = String::new();
                    match TcpStream::connect(AP_SERVER_ADDRESS) {
                        Ok(mut ap_stream) => {
                            let command = format!("FORWARD_REQUEST:{}\n", peer_addr);
                            let _ = ap_stream.write_all(command.as_bytes());
                            let _ = ap_stream.flush();

                            let mut ap_buffer = [0; 2048];
                            // これで Read トレイトがスコープに入る
                            if let Ok(br) = ap_stream.read(&mut ap_buffer) {
                                ap_report = String::from_utf8_lossy(&ap_buffer[..br]).to_string();
                            }
                            let _ = led.set_high();
                        }
                        Err(e) => {
                            log::error!("[Web Tier] AP接続失敗: {}", e);
                            ap_report = format!(
                                "<span style='color:#ef4444;'>[AP Error] ❌ AP Tier Cluster Down ({})</span>",
                                e
                            );
                        }
                    }

                    // ログ1行分のHTMLパーツ
                    let log_line = format!(
                        "<div class='log-entry'><span class='badge'>[LIVE]</span> 📡 Client: {} ➡️ AP Layer ({}) ➡️ AP Response: {}</div>",
                        peer_addr, AP_SERVER_ADDRESS, ap_report
                    );

                    let response = format!(
                        "HTTP/1.1 200 OK\r\n\
                         Content-Type: text/html; charset=UTF-8\r\n\
                         Content-Length: {}\r\n\
                         Access-Control-Allow-Origin: *\r\n\
                         Connection: close\r\n\
                         \r\n\
                         {}",
                        log_line.len(),
                        log_line
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();

                    thread::sleep(Duration::from_millis(100));
                    let _ = led.set_low();
                    continue;
                }

                // 初回アクセス（ブラウザでURLを叩いたとき）に返す、土台となるターミナルUI
                log::info!(
                    "[Web Tier] ブラウザがメインコンソールに接続しました: {}",
                    peer_addr
                );

                let response_body = format!(
                    "<!DOCTYPE html><html><head><meta charset='utf-8'><title>Distributed Core Console</title>\
                    <style>\
                        body{{background:#090d16;color:#38bdf8;font-family:'Courier New',monospace;padding:20px;margin:0;}}\
                        .console{{background:#020617;border:1px solid #1e293b;border-radius:8px;padding:20px;box-shadow:0 0 20px rgba(56,189,248,0.15);max-width:900px;margin:20px auto;}}\
                        .header{{border-bottom:1px dashed #1e293b;padding-bottom:10px;margin-bottom:15px;display:flex;justify-content:space-between;color:#94a3b8;font-size:12px;}}\
                        .prompt{{color:#34d399;font-weight:bold;}}\
                        .blink{{animation:blink 1s infinite;color:#38bdf8;}}\
                        @keyframes blink{{0%,100%{{opacity:1}}50%{{opacity:0}}}}\
                        .log-container{{max-height:400px;overflow-y:auto;margin:15px 0;padding-right:5px;}}\
                        .log-entry{{margin:8px 0;line-height:1.4;font-size:13px;animation:fadeIn 0.2s ease;color:#bae6fd;border-bottom:1px solid #1e293b;padding-bottom:4px;}}\
                        @keyframes fadeIn{{from{{opacity:0;transform:translateY(2px)}}to{{opacity:1;transform:translateY(0)}}}}\
                        .badge{{background:#1e1b4b;color:#818cf8;padding:2px 6px;border-radius:4px;font-size:11px;border:1px solid #312e81;}}\
                        ul{{margin:0;padding:0 0 0 15px;display:inline-block;vertical-align:top;}}\
                        li{{color:#34d399;list-style:none;display:inline;}}\
                    </style>\
                    <script>\
                        function fetchLog() {{\
                            console.log('Fetching live log...');\
                            fetch('/api/stream')\
                                .then(response => {{\
                                    if(!response.ok) throw new Error('Network response error');\
                                    return response.text();\
                                }})\
                                .then(html => {{\
                                    if(html.trim().length > 0) {{\
                                        const container = document.getElementById('log-box');\
                                        container.insertAdjacentHTML('beforeend', html);\
                                        container.scrollTop = container.scrollHeight;\
                                    }}\
                                }})\
                                .catch(err => console.error('Fetch failed:', err));\
                        }}\
                        window.onload = function() {{\
                            fetchLog();\
                            setInterval(fetchLog, 5000);\
                        }};\
                    </script></head>\
                    <body><div class='console'>\
                        <div class='header'>\
                            <span>CLUSTER CORE NETWORK MONITOR v3.2</span>\
                            <span>STREAMING: LIVE <span class='blink' style='color:#ef4444;'>●</span></span>\
                        </div>\
                        <div class='log-entry'><span class='prompt'>tatsuya@esp32-cluster:~$</span> tail -f /var/log/cluster/mesh.log</div>\
                        <div class='log-entry' style='color:#34d399;'>🟢 [System] 1台目(Web)開通完了。ブロックフリーTCPストリーミング同期中...</div>\
                        \
                        \
                        <div id='log-box' class='log-container'></div>\
                        \
                        <div class='log-entry'><span class='prompt'>tatsuya@esp32-cluster:~$</span> <span class='blink'>_</span></div>\
                    </div></body></html>"
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

                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
            Err(e) => log::error!("[Web Tier] 接続エラー: {}", e),
        }
    }
    Ok(())
}
