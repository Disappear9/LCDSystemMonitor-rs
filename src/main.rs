use serialport::SerialPort;
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::System;
use sysinfo::Networks;
use clap::Parser;
use config::Config;
use std::collections::HashMap;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// 串口设备路径
    #[arg(short, long)]
    serial_port: Option<String>,
    
    /// 网络接口名称
    #[arg(short, long)]
    network_interface: Option<String>,
    
    /// 刷新间隔（秒）
    #[arg(short, long, default_value_t = 3)]
    refresh_interval: u64,
    
    /// 配置文件路径
    #[arg(short = 'c', long, default_value = "config.toml")]
    config_file: String,
}

#[derive(Debug)]
struct AppConfig {
    serial_port: String,
    network_interface: String,
    refresh_interval: u64,
}

struct LCDDisplay {
    port: Box<dyn SerialPort>,
}

impl AppConfig {
    fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let cli = Cli::parse();
        
        // 尝试从配置文件读取
        let config_builder = Config::builder()
            .add_source(config::File::with_name(&cli.config_file).required(false))
            .add_source(config::Environment::with_prefix("APP"));
            
        let config_map = match config_builder.build() {
            Ok(config) => config.try_deserialize::<HashMap<String, String>>().unwrap_or_default(),
            Err(_) => HashMap::new(),
        };
        
        // 优先级：命令行参数 > 配置文件 > 默认值
        let serial_port = cli.serial_port
            .or_else(|| config_map.get("serial_port").map(|s| s.to_string()))
            .unwrap_or_else(|| {
                if cfg!(target_os = "windows") {
                    "COM3".to_string()
                } else {
                    "/dev/ttyUSB0".to_string()
                }
            });
            
        let network_interface = cli.network_interface
            .or_else(|| config_map.get("network_interface").map(|s| s.to_string()))
            .unwrap_or_else(|| {
                if cfg!(target_os = "windows") {
                    "Ethernet".to_string()
                } else {
                    "eth0".to_string()
                }
            });
            
        let refresh_interval = cli.refresh_interval;
        
        Ok(AppConfig {
            serial_port,
            network_interface,
            refresh_interval,
        })
    }
}

impl LCDDisplay {
    fn new(port_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let builder = serialport::new(port_name, 9600)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(Duration::from_millis(100));

        let port = builder.open()?;
        
        Ok(LCDDisplay { port })
    }

    fn clear_screen(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.send_command(0x01)?;
        thread::sleep(Duration::from_millis(2)); // 清屏需要额外时间
        Ok(())
    }

    fn set_cursor_position(&mut self, line: u8, column: u8) -> Result<(), Box<dyn std::error::Error>> {
        let address = match line {
            0 => 0x80 + column,  // 第一行地址从 0x80 开始
            1 => 0xC0 + column,  // 第二行地址从 0xC0 开始
            _ => 0x80,
        };
        self.send_command(address)
    }

    fn send_command(&mut self, cmd: u8) -> Result<(), Box<dyn std::error::Error>> {
        self.port.write_all(&[0xFE, cmd])?;
        Ok(())
    }

    fn write_string(&mut self, text: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.port.write_all(text.as_bytes())?;
        Ok(())
    }

    fn display_text(&mut self, line1: &str, line2: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.clear_screen()?;
        
        // 显示第一行，居中对齐
        let line1_padded = Self::center_align_text(line1, 16);
        self.set_cursor_position(0, 0)?;
        self.write_string(&line1_padded)?;
        
        // 显示第二行，居中对齐
        let line2_padded = Self::center_align_text(line2, 16);
        self.set_cursor_position(1, 0)?;
        self.write_string(&line2_padded)?;
        
        Ok(())
    }

    fn center_align_text(text: &str, width: usize) -> String {
        let text_len = text.chars().count();
        if text_len >= width {
            return text.chars().take(width).collect();
        }
        
        let padding = (width - text_len) / 2;
        let mut result = " ".repeat(padding);
        result.push_str(text);
        result
    }
}

struct SystemMonitor {
    system: System,
    network_interface: String,
    prev_network_data: (u64, u64),
    prev_network_time: Instant,
}

impl SystemMonitor {
    fn new(network_interface: &str) -> Self {
        let mut system = System::new_all();
        system.refresh_all();
        
        // 获取初始网络数据
        let networks = Networks::new_with_refreshed_list();
        let mut prev_received = 0;
        let mut prev_transmitted = 0;
        
        for (interface_name, network) in &networks {
            if interface_name == network_interface {
                prev_received = network.total_received();
                prev_transmitted = network.total_transmitted();
                break;
            }
        }
        
        SystemMonitor {
            system,
            network_interface: network_interface.to_string(),
            prev_network_data: (prev_received, prev_transmitted),
            prev_network_time: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        self.system.refresh_all();
    }

    fn get_cpu_usage(&self) -> f32 {
        self.system.global_cpu_usage()
    }

    fn get_memory_usage(&self) -> (u64, u64) {
        let used_memory = self.system.used_memory();
        let total_memory = self.system.total_memory();
        (used_memory, total_memory)
    }

    fn get_network_speed(&mut self) -> (f64, f64) {
        let current_time = Instant::now();
        let elapsed = current_time.duration_since(self.prev_network_time).as_secs_f64();
        
        let networks = Networks::new_with_refreshed_list();
        let mut current_received = 0;
        let mut current_transmitted = 0;
        
        // 查找指定网络接口
        for (interface_name, network) in &networks {
            if *interface_name == self.network_interface {
                current_received = network.total_received();
                current_transmitted = network.total_transmitted();
                break;
            }
        }
        
        let (prev_received, prev_transmitted) = self.prev_network_data;
        
        let download_speed = if current_received >= prev_received {
            (current_received - prev_received) as f64 / elapsed
        } else {
            0.0
        };
        
        let upload_speed = if current_transmitted >= prev_transmitted {
            (current_transmitted - prev_transmitted) as f64 / elapsed
        } else {
            0.0
        };
        
        self.prev_network_data = (current_received, current_transmitted);
        self.prev_network_time = current_time;
        
        (download_speed, upload_speed)
    }

    fn get_hostname(&self) -> String {
        sysinfo::System::host_name().unwrap_or_else(|| "Unknown".to_string())
    }

    fn get_uptime(&self) -> String {
        let uptime_seconds = sysinfo::System::uptime();
        let days = uptime_seconds / (24 * 3600);
        let hours = (uptime_seconds % (24 * 3600)) / 3600;
        let minutes = (uptime_seconds % 3600) / 60;
        
        if days > 0 {
            format!("{}d {:02}:{:02}h", days, hours, minutes)
        } else {
            format!("{:02}:{:02}:{:02}h", hours, minutes, uptime_seconds % 60)
        }
    }

    fn format_speed(speed: f64) -> String {
        if speed >= 1_000_000.0 {
            format!("{:.1}MB/s", speed / 1_000_000.0)
        } else if speed >= 1_000.0 {
            format!("{:.1}KB/s", speed / 1_000.0)
        } else {
            format!("{:.0}B/s", speed)
        }
    }

    fn format_memory(used: u64, total: u64) -> String {
        let used_gb = used as f64 / 1_073_741_824.0; // 转换为GB
        let percentage = (used as f64 / total as f64 * 100.0) as u32;
        
        format!("{:.1}GB @{}%", used_gb, percentage)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = AppConfig::load()?;

    println!("初始化串口 LCD 显示器: {}", config.serial_port);
    let mut lcd = LCDDisplay::new(&config.serial_port)?;
    
    println!("初始化系统监控，网络接口: {}", config.network_interface);
    let mut monitor = SystemMonitor::new(&config.network_interface);

    // 初始显示
    lcd.display_text("System Monitor", "Initializing...")?;
    thread::sleep(Duration::from_secs(2));
    
    let mut display_state = 0;
    
    loop {
        monitor.refresh();
        
        match display_state {
            0 => {
                // 显示 CPU 和内存信息
                let cpu_usage = monitor.get_cpu_usage();
                let (used_memory, total_memory) = monitor.get_memory_usage();
                let memory_info = SystemMonitor::format_memory(used_memory, total_memory);
                
                lcd.display_text(
                    &format!("CPU: {:.1}%", cpu_usage),
                    &format!("Mem: {}", memory_info),
                )?;
            }
            1 => {
                // 显示网络速度
                let (download, upload) = monitor.get_network_speed();
                
                lcd.display_text(
                    &format!("Down: {}", SystemMonitor::format_speed(download)),
                    &format!("Up: {}", SystemMonitor::format_speed(upload)),
                )?;
            }
            _ => {
                // 显示主机名和系统运行时间
                let hostname = monitor.get_hostname();
                let uptime = monitor.get_uptime();
                
                // 截断主机名以适应屏幕宽度
                let hostname_short = if hostname.len() > 16 {
                    &hostname[..16]
                } else {
                    &hostname
                };
                
                lcd.display_text(
                    &format!("{}", hostname_short),
                    &format!("Up: {}", uptime),
                )?;
            }
        }
        
        // 切换显示状态
        display_state = (display_state + 1) % 3;
        
        // 等待一段时间后刷新
        thread::sleep(Duration::from_secs(config.refresh_interval));
    }
}
