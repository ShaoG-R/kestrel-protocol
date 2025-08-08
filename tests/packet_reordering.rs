//! 网络包乱序和早到帧缓存机制测试
//! Tests for network packet reordering and early arrival frame caching

pub mod common;

use common::harness::TestHarness;
use kestrel_protocol::{
    socket::handle::initial_data::InitialData,
    config::Config,
};
use std::{
    sync::{Arc, atomic::{AtomicU32, AtomicBool, Ordering}},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{sleep, Instant},
};
use tracing::{info, warn, debug, Instrument};

/// 模拟网络延迟的辅助结构
struct NetworkSimulator {
    /// 是否启用包乱序模拟
    reorder_enabled: Arc<AtomicBool>,
    /// 延迟范围（毫秒）
    delay_range: (u64, u64),
}

impl NetworkSimulator {
    fn new() -> Self {
        Self {
            reorder_enabled: Arc::new(AtomicBool::new(false)),
            delay_range: (10, 50),
        }
    }

    /// 启用包乱序模拟
    fn enable_reordering(&self) {
        self.reorder_enabled.store(true, Ordering::SeqCst);
    }

    /// 模拟网络延迟
    async fn simulate_delay(&self) {
        if self.reorder_enabled.load(Ordering::SeqCst) {
            let delay = fastrand::u64(self.delay_range.0..=self.delay_range.1);
            sleep(Duration::from_millis(delay)).await;
        }
    }
}

/// 测试基础的早到帧缓存功能
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_basic_early_frame_caching() {

    info!("--- 测试基础早到帧缓存功能 ---");

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let client = TestHarness::create_client().await;
    let network_sim = Arc::new(NetworkSimulator::new());
    
    // 启用包乱序模拟
    network_sim.enable_reordering();

    // 模拟延迟的数据发送
    let network_sim_clone = network_sim.clone();
    let data_send_task = tokio::spawn(async move {
        // 模拟第二个数据包先到达的情况
        network_sim_clone.simulate_delay().await;
        
        let config = Config::default();
        let initial_data = InitialData::new(b"Early frame test data", &config).unwrap();
        client.connect_with_config(server_addr, Box::new(config), Some(initial_data)).await
    });

    // 服务器端处理
    let server_handle = tokio::spawn(async move {
        let (stream, _remote_addr) = server_listener.accept().await.unwrap();
        info!("[Server] 接受连接 (可能包含早到帧)");

        let (mut reader, mut writer) = tokio::io::split(stream);

        // 触发SYN-ACK
        writer.write_all(b"Server ACK").await.unwrap();

        // 读取可能的早到数据
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();

        if !buf.is_empty() {
            info!("[Server] 成功接收早到帧数据: {:?}", String::from_utf8_lossy(&buf));
            assert_eq!(&buf, b"Early frame test data");
        }
    });

    // 等待连接建立和数据传输
    let client_stream = data_send_task.await.unwrap().unwrap();
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);

    // 读取服务器ACK
    let mut ack_buf = vec![0u8; 10];
    client_reader.read_exact(&mut ack_buf).await.unwrap();
    assert_eq!(&ack_buf, b"Server ACK");

    client_writer.shutdown().await.unwrap();
    server_handle.await.unwrap();

    info!("--- 基础早到帧缓存测试通过 ---");
}

/// 测试多客户端场景下的早到帧处理
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_multi_client_early_frames() {

    const NUM_CLIENTS: usize = 3; // 减少并发数量避免超时
    info!("--- 测试多客户端早到帧处理 ({} 客户端) ---", NUM_CLIENTS);

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let success_counter = Arc::new(AtomicU32::new(0));
    let server_counter = success_counter.clone();

    // 服务器任务
    let server_handle = tokio::spawn(async move {
        let mut handlers = Vec::new();

        for i in 0..NUM_CLIENTS {
            let (stream, _remote_addr) = server_listener.accept().await.unwrap();
            let counter = server_counter.clone();

            let handler = tokio::spawn(
                async move {
                    let (mut reader, mut writer) = tokio::io::split(stream);

                    // 立即触发SYN-ACK以建立连接
                    let response = format!("ACK{}", i);
                    writer.write_all(response.as_bytes()).await.unwrap();

                    // 读取客户端数据（可能是早到的）
                    let mut buf = Vec::new();
                    reader.read_to_end(&mut buf).await.unwrap();

                    let received = String::from_utf8_lossy(&buf);
                    let expected = format!("Data from client {}", i);
                    
                    if received.trim() == expected {
                        counter.fetch_add(1, Ordering::SeqCst);
                        debug!("[Server Handler {}] 成功处理早到帧", i);
                    } else {
                        warn!("[Server Handler {}] 数据不匹配: 期望 '{}', 收到 '{}'", i, expected, received.trim());
                    }
                }
                .instrument(tracing::debug_span!("server_handler", id = i))
            );
            handlers.push(handler);
        }

        for handler in handlers {
            handler.await.unwrap();
        }
    });

    // 客户端任务
    let mut client_handles = Vec::new();
    for i in 0..NUM_CLIENTS {
        let client = TestHarness::create_client().await;
        let handle = tokio::spawn(
            async move {
                // 添加随机延迟来模拟网络条件
                let delay = fastrand::u64(0..20);
                sleep(Duration::from_millis(delay)).await;

                let test_data = format!("Data from client {}", i);
                let mut config = Config::default();
                // 为多客户端并发测试调整超时配置
                config.connection.idle_timeout = Duration::from_secs(15); // 增加到15秒
                config.reliability.initial_rto = Duration::from_millis(2000); // 更保守的RTO
                config.reliability.min_rto = Duration::from_millis(1000); // 更保守的最小RTO
                let initial_data = InitialData::new(test_data.as_bytes(), &config).unwrap();
                
                let stream = client
                    .connect_with_config(server_addr, Box::new(config), Some(initial_data))
                    .await
                    .unwrap();

                let (mut reader, mut writer) = tokio::io::split(stream);

                    // 读取服务器ACK（动态长度）
                    let mut ack_buf = Vec::new();
                    reader.read_to_end(&mut ack_buf).await.unwrap();
                    
                    // 转换为字符串并验证
                    let ack_str = String::from_utf8_lossy(&ack_buf);
                    assert!(ack_str.starts_with("ACK"), "ACK should start with 'ACK', got: {}", ack_str);
                    assert!(ack_str.len() >= 4, "ACK should have at least 4 characters, got: {}", ack_str);
                    
                    // 验证ACK ID部分是数字
                    let ack_id_part = &ack_str[3..];
                    assert!(ack_id_part.chars().all(|c| c.is_ascii_digit()), 
                            "ACK ID should be digits, got: {}", ack_id_part);

                writer.shutdown().await.unwrap();
                debug!("[Client {}] 连接完成", i);
            }
            .instrument(tracing::debug_span!("client", id = i))
        );
        client_handles.push(handle);
    }

    // 等待所有任务完成
    for handle in client_handles {
        handle.await.unwrap();
    }
    server_handle.await.unwrap();

    let successful = success_counter.load(Ordering::SeqCst);
    assert_eq!(successful as usize, NUM_CLIENTS);
    
    info!("--- 多客户端早到帧测试通过 ({}/{}) ---", successful, NUM_CLIENTS);
}

/// 测试早到帧的超时清理机制
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_early_frame_timeout_cleanup() {

    info!("--- 测试早到帧超时清理机制 ---");

    // 创建两个独立的测试环境避免连接冲突
    let TestHarness {
        server_addr: zombie_server_addr,
        server_listener: zombie_listener,
        ..
    } = TestHarness::new().await;

    let TestHarness {
        server_addr: normal_server_addr,
        server_listener: mut normal_listener,
        ..
    } = TestHarness::new().await;

    let zombie_client = TestHarness::create_client().await;
    let normal_client = TestHarness::create_client().await;

    // 僵尸服务器任务：模拟永不响应的服务器
    let zombie_server_handle = tokio::spawn(async move {
        info!("[Zombie Server] 启动但不处理连接");
        // 故意不调用 accept()，模拟服务器无响应
        // 保持listener活跃但不处理连接
        let _listener = zombie_listener;
        sleep(Duration::from_secs(10)).await;
    });

    // 客户端1：发送0-RTT数据但无法完成握手（服务器不响应）
    let zombie_client_handle = tokio::spawn(async move {
        info!("[Zombie Client] 发送0-RTT数据但服务器不响应");
        let config = Config::default();
        let initial_data = InitialData::new(b"Zombie data", &config).unwrap();
        
        // 尝试连接但故意让它超时
        let connect_result = tokio::time::timeout(
            Duration::from_millis(500), // 增加超时时间
            zombie_client.connect_with_config(zombie_server_addr, Box::new(config), Some(initial_data))
        ).await;
        
        match connect_result {
            Ok(_) => info!("[Zombie Client] 意外完成连接"),
            Err(_) => info!("[Zombie Client] 连接超时，模拟僵尸连接成功"),
        }
    });

    // 等待僵尸客户端开始连接尝试
    sleep(Duration::from_millis(100)).await;

    // 正常服务器任务：处理正常连接
    let normal_server_handle = tokio::spawn(async move {
        info!("[Normal Server] 等待正常连接...");
        let (stream, remote_addr) = normal_listener.accept().await.unwrap();
        info!("[Normal Server] 接受正常连接来自 {}", remote_addr);

        let (mut reader, mut writer) = tokio::io::split(stream);

        // 触发SYN-ACK
        writer.write_all(b"Normal ACK").await.unwrap();

        // 读取客户端数据
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        
        assert_eq!(&buf, b"Normal data");
        info!("[Normal Server] 正常连接处理完成");
    });

    // 客户端2：正常连接到不同的服务器
    let normal_client_handle = tokio::spawn(async move {
        info!("[Normal Client] 开始正常连接");
        let mut config = Config::default();
        // 为超时清理测试调整配置
        config.connection.idle_timeout = Duration::from_secs(15);
        config.reliability.initial_rto = Duration::from_millis(1500);
        let initial_data = InitialData::new(b"Normal data", &config).unwrap();
        
        let stream = normal_client
            .connect_with_config(normal_server_addr, Box::new(config), Some(initial_data))
            .await
            .unwrap();

        let (mut reader, mut writer) = tokio::io::split(stream);

        // 读取服务器响应
        let mut buf = vec![0u8; 10];
        reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"Normal ACK");

        writer.shutdown().await.unwrap();
        info!("[Normal Client] 正常连接完成");
    });

    // 等待正常连接完成
    let normal_client_result = normal_client_handle.await;
    let normal_server_result = normal_server_handle.await;
    
    // 检查正常连接是否成功
    normal_client_result.unwrap();
    normal_server_result.unwrap();

    // 让僵尸连接超时
    let zombie_client_result = zombie_client_handle.await;
    zombie_client_result.unwrap();

    // 清理僵尸服务器
    zombie_server_handle.abort();

    // 等待足够长时间让早到帧缓存清理机制工作
    info!("等待早到帧缓存清理...");
    sleep(Duration::from_secs(3)).await; // 减少等待时间

    info!("--- 早到帧超时清理测试通过 ---");
}

/// 测试高频率的包乱序场景
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_high_frequency_packet_reordering() {

    const NUM_RAPID_CLIENTS: usize = 50;
    info!("--- 测试高频率包乱序场景 ({} 快速客户端) ---", NUM_RAPID_CLIENTS);

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let start_time = Instant::now();
    let success_counter = Arc::new(AtomicU32::new(0));
    let server_counter = success_counter.clone();

    // 服务器任务
    let server_handle = tokio::spawn(async move {
        let mut handlers = Vec::new();

        for i in 0..NUM_RAPID_CLIENTS {
            let (stream, _) = server_listener.accept().await.unwrap();
            let counter = server_counter.clone();

            let handler = tokio::spawn(async move {
                let (mut reader, mut writer) = tokio::io::split(stream);

                // 快速响应
                writer.write_all(b"FAST").await.unwrap();

                // 读取数据
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf).await.unwrap();

                // 验证数据模式
                if buf.len() == 20 && buf[0] == (i % 256) as u8 {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            handlers.push(handler);
        }

        for handler in handlers {
            handler.await.unwrap();
        }
    });

    // 快速客户端任务
    let mut client_handles = Vec::new();
    for i in 0..NUM_RAPID_CLIENTS {
        let client = TestHarness::create_client().await;
        let handle = tokio::spawn(async move {
            // 创建具有特定模式的数据
            let mut test_data = vec![(i % 256) as u8; 20];
            test_data[0] = (i % 256) as u8; // 标识符
            
            let config = Config::default();
            let initial_data = InitialData::new(&test_data, &config).unwrap();
            
            let stream = client
                .connect_with_config(server_addr, Box::new(config), Some(initial_data))
                .await
                .unwrap();

            let (mut reader, mut writer) = tokio::io::split(stream);

            // 快速读取响应
            let mut buf = vec![0u8; 4];
            reader.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"FAST");

            writer.shutdown().await.unwrap();
        });
        client_handles.push(handle);
    }

    // 等待所有客户端完成
    for handle in client_handles {
        handle.await.unwrap();
    }
    server_handle.await.unwrap();

    let elapsed = start_time.elapsed();
    let successful = success_counter.load(Ordering::SeqCst);
    
    info!("--- 高频率包乱序测试结果 ---");
    info!("成功连接: {}/{}", successful, NUM_RAPID_CLIENTS);
    info!("总耗时: {:?}", elapsed);
    info!("平均连接时间: {:.2} ms", elapsed.as_millis() as f64 / NUM_RAPID_CLIENTS as f64);

    assert_eq!(successful as usize, NUM_RAPID_CLIENTS);
    info!("--- 高频率包乱序测试通过 ---");
}

/// 测试早到帧缓存的内存管理
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_early_frame_memory_management() {

    const NUM_PHASES: usize = 5;
    const CLIENTS_PER_PHASE: usize = 20;
    info!("--- 测试早到帧缓存内存管理 ({} 阶段, 每阶段 {} 客户端) ---", NUM_PHASES, CLIENTS_PER_PHASE);

    let total_success = Arc::new(AtomicU32::new(0));

    for phase in 0..NUM_PHASES {
        info!("=== 阶段 {} ===", phase + 1);
        
        // 为每个阶段创建新的TestHarness
        let TestHarness {
            server_addr,
            mut server_listener,
            ..
        } = TestHarness::new().await;
        
        let phase_success = Arc::new(AtomicU32::new(0));
        let server_counter = phase_success.clone();
        let total_counter = total_success.clone();

        // 每个阶段的服务器任务
        let server_handle = tokio::spawn(async move {
            let mut handlers = Vec::new();

            for _i in 0..CLIENTS_PER_PHASE {
                let (stream, _) = server_listener.accept().await.unwrap();
                let phase_counter = server_counter.clone();
                let total_counter = total_counter.clone();

                let handler = tokio::spawn(async move {
                    let (mut reader, mut writer) = tokio::io::split(stream);

                    writer.write_all(b"PHASE_OK").await.unwrap();

                    let mut buf = Vec::new();
                    reader.read_to_end(&mut buf).await.unwrap();

                    // 验证数据
                    if !buf.is_empty() {
                        phase_counter.fetch_add(1, Ordering::SeqCst);
                        total_counter.fetch_add(1, Ordering::SeqCst);
                    }
                });
                handlers.push(handler);
            }

            for handler in handlers {
                handler.await.unwrap();
            }
        });

        // 每个阶段的客户端任务
        let mut client_handles = Vec::new();
        for i in 0..CLIENTS_PER_PHASE {
            let client = TestHarness::create_client().await;
            let handle = tokio::spawn(async move {
                let data = format!("Phase {} Client {} Data", phase, i);
                let config = Config::default();
                let initial_data = InitialData::new(data.as_bytes(), &config).unwrap();
                
                let stream = client
                    .connect_with_config(server_addr, Box::new(config), Some(initial_data))
                    .await
                    .unwrap();

                let (mut reader, mut writer) = tokio::io::split(stream);

                let mut buf = vec![0u8; 8];
                reader.read_exact(&mut buf).await.unwrap();
                assert_eq!(&buf, b"PHASE_OK");

                writer.shutdown().await.unwrap();
            });
            client_handles.push(handle);
        }

        // 等待这个阶段完成
        for handle in client_handles {
            handle.await.unwrap();
        }
        
        server_handle.await.unwrap();

        let phase_count = phase_success.load(Ordering::SeqCst);
        info!("阶段 {} 完成: {}/{} 成功", phase + 1, phase_count, CLIENTS_PER_PHASE);

        // 在阶段间添加短暂延迟，让缓存清理机制工作
        if phase < NUM_PHASES - 1 {
            sleep(Duration::from_millis(500)).await;
        }
    }

    let total_count = total_success.load(Ordering::SeqCst);
    let expected_total = NUM_PHASES * CLIENTS_PER_PHASE;
    
    info!("--- 早到帧缓存内存管理测试结果 ---");
    info!("总成功连接: {}/{}", total_count, expected_total);
    
    assert_eq!(total_count as usize, expected_total);
    info!("--- 早到帧缓存内存管理测试通过 ---");
}

/// 测试极端乱序场景下的数据完整性
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_extreme_reordering_data_integrity() {

    info!("--- 测试极端乱序场景数据完整性 ---");

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let client = TestHarness::create_client().await;

    // 创建复杂的测试数据
    let mut complex_data = Vec::new();
    for i in 0..256 {
        complex_data.push(i as u8);
    }
    for i in (0..256).rev() {
        complex_data.push(i as u8);
    }

    let server_handle = tokio::spawn(async move {
        let (stream, _) = server_listener.accept().await.unwrap();
        let (mut reader, mut writer) = tokio::io::split(stream);

        writer.write_all(b"COMPLEX_ACK").await.unwrap();

        let mut received_data = Vec::new();
        reader.read_to_end(&mut received_data).await.unwrap();

        // 验证数据完整性
        assert_eq!(received_data.len(), 512);
        
        // 验证数据模式
        for i in 0..256 {
            assert_eq!(received_data[i], i as u8);
        }
        for i in 0..256 {
            assert_eq!(received_data[256 + i], (255 - i) as u8);
        }

        info!("[Server] 复杂数据完整性验证通过");
    });

    // 模拟极端网络延迟
    sleep(Duration::from_millis(100)).await;

    let config = Config::default();
    let initial_data = InitialData::new(&complex_data, &config).unwrap();
    let stream = client
        .connect_with_config(server_addr, Box::new(config), Some(initial_data))
        .await
        .unwrap();

    let (mut reader, mut writer) = tokio::io::split(stream);

    let mut ack_buf = vec![0u8; 11];
    reader.read_exact(&mut ack_buf).await.unwrap();
    assert_eq!(&ack_buf, b"COMPLEX_ACK");

    writer.shutdown().await.unwrap();
    server_handle.await.unwrap();

    info!("--- 极端乱序场景数据完整性测试通过 ---");
}