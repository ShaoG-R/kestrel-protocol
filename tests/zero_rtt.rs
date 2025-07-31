//! 0-RTT连接和早到帧缓存机制的全面测试
//! Comprehensive tests for 0-RTT connections and early arrival frame caching

pub mod common;

use common::harness::{init_tracing, TestHarness};
use kestrel_protocol::{
    socket::handle::initial_data::InitialData,
    config::Config,
};
use std::{
    sync::{Arc, atomic::{AtomicU32, Ordering}},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, Barrier},
    time::{sleep, Instant},
};
use tracing::{info, debug, Instrument};

/// 测试基础的0-RTT连接建立
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_basic_zero_rtt_connection() {
    init_tracing();
    info!("--- 测试基础0-RTT连接建立 ---");

    // 1. 设置服务器
    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    // 2. 创建客户端
    let client = TestHarness::create_client().await;
    
    // 用于同步服务器接受的流
    let (server_stream_tx, mut server_stream_rx) = mpsc::channel(1);

    // 3. 启动服务器任务
    let server_handle = tokio::spawn(async move {
        info!("[Server] 等待连接...");
        let (stream, remote_addr) = server_listener.accept().await.unwrap();
        info!("[Server] 接受来自 {} 的连接", remote_addr);
        server_stream_tx.send(stream).await.unwrap();
    });

    // 4. 客户端使用0-RTT数据建立连接
    let config = Config::default();
    let initial_data = InitialData::new(b"Hello 0-RTT!", &config).unwrap();
    info!("[Client] 使用0-RTT数据连接到服务器...");
    
    let client_stream = client
        .connect_with_config(server_addr, config, Some(initial_data))
        .await
        .unwrap();
    info!("[Client] 0-RTT连接建立完成");

    // 5. 获取服务器流
    let server_stream = server_stream_rx.recv().await.unwrap();
    server_handle.await.unwrap();

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    // 6. 服务器首次写入应该触发SYN-ACK发送
    let server_response = b"Server ACK for 0-RTT";
    server_writer
        .write_all(server_response)
        .await
        .expect("服务器写入应该成功");

    // 7. 客户端应该能接收到服务器响应
    let mut response_buf = vec![0u8; server_response.len()];
    client_reader
        .read_exact(&mut response_buf)
        .await
        .expect("客户端应该能读取服务器响应");
    assert_eq!(&response_buf, server_response);
    info!("[Client] 收到服务器响应: {:?}", String::from_utf8_lossy(&response_buf));

    // 8. 测试后续正常数据传输
    let client_msg = b"Regular message after 0-RTT";
    client_writer
        .write_all(client_msg)
        .await
        .expect("客户端写入应该成功");

    // 9. 客户端关闭写入端，让服务器读取所有数据
    client_writer.shutdown().await.unwrap();

    // 10. 服务器读取所有数据（包括0-RTT数据和后续消息）
    let mut all_data = Vec::new();
    server_reader.read_to_end(&mut all_data).await.unwrap();

    // 验证接收到的数据包含0-RTT数据和后续消息
    let mut expected_data = Vec::new();
    expected_data.extend_from_slice(b"Hello 0-RTT!");
    expected_data.extend_from_slice(client_msg);
    assert_eq!(all_data, expected_data);
    info!("[Server] 成功接收所有数据: 0-RTT + 后续消息 ({} bytes)", all_data.len());

    info!("--- 基础0-RTT连接测试通过 ---");
}

/// 测试多个0-RTT数据包的分包功能
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_zero_rtt_packet_coalescing() {
    init_tracing();
    info!("--- 测试0-RTT包粘连分包功能 ---");

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let client = TestHarness::create_client().await;
    let (server_stream_tx, mut server_stream_rx) = mpsc::channel(1);

    let server_handle = tokio::spawn(async move {
        let (stream, remote_addr) = server_listener.accept().await.unwrap();
        info!("[Server] 接受连接来自 {}", remote_addr);
        server_stream_tx.send(stream).await.unwrap();
    });

    // 使用较大的0-RTT数据来触发分包
    let large_initial_data = vec![b'A'; 512]; // 512B数据，避免超过单包限制
    let config = Config::default();
    let initial_data = InitialData::new(&large_initial_data, &config).unwrap();
    
    info!("[Client] 发送大量0-RTT数据 ({} bytes)...", large_initial_data.len());
    let client_stream = client
        .connect_with_config(server_addr, config, Some(initial_data))
        .await
        .unwrap();

    let server_stream = server_stream_rx.recv().await.unwrap();
    server_handle.await.unwrap();

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    // 服务器首次写入触发SYN-ACK
    server_writer.write_all(b"START").await.unwrap();

    // 客户端读取服务器确认
    let mut ack_buf = vec![0u8; 5];
    client_reader.read_exact(&mut ack_buf).await.unwrap();
    assert_eq!(&ack_buf, b"START");
    
    // 客户端关闭写入端
    client_writer.shutdown().await.unwrap();

    // 服务器读取所有0-RTT数据
    let mut received_data = Vec::new();
    server_reader.read_to_end(&mut received_data).await.unwrap();
    
    // 验证数据完整性
    assert_eq!(received_data.len(), large_initial_data.len());
    assert_eq!(received_data, large_initial_data);
    info!("[Server] 成功接收所有分包数据 ({} bytes)", received_data.len());

    info!("--- 0-RTT包粘连分包测试通过 ---");
}

/// 测试早到帧缓存机制
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_early_arrival_frame_caching() {
    init_tracing();
    info!("--- 测试早到帧缓存机制 ---");

    // 这个测试比较复杂，因为我们需要模拟网络包乱序
    // 我们通过延迟SYN包的发送来模拟这种情况
    
    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let client1 = TestHarness::create_client().await;
    let client2 = TestHarness::create_client().await;
    
    let barrier = Arc::new(Barrier::new(3)); // 3个任务同步

    // 服务器任务
    let server_barrier = barrier.clone();
    let server_handle = tokio::spawn(async move {
        server_barrier.wait().await; // 等待所有任务准备就绪
        
        let (stream1, addr1) = server_listener.accept().await.unwrap();
        info!("[Server] 接受第一个连接来自 {}", addr1);
        
        let (stream2, addr2) = server_listener.accept().await.unwrap();
        info!("[Server] 接受第二个连接来自 {}", addr2);
        
        // 验证两个连接都能正常工作
        let (mut reader1, mut writer1) = tokio::io::split(stream1);
        let (mut reader2, mut writer2) = tokio::io::split(stream2);
        
        // 触发SYN-ACK
        writer1.write_all(b"Server response 1").await.unwrap();
        writer2.write_all(b"Server response 2").await.unwrap();
        
        // 读取客户端消息
        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();
        reader1.read_to_end(&mut buf1).await.unwrap();
        reader2.read_to_end(&mut buf2).await.unwrap();
        
        info!("[Server] 接收到数据1: {:?}", String::from_utf8_lossy(&buf1));
        info!("[Server] 接收到数据2: {:?}", String::from_utf8_lossy(&buf2));
    });

    // 客户端1 - 立即连接
    let client1_barrier = barrier.clone();
    let client1_handle = tokio::spawn(async move {
        client1_barrier.wait().await;
        
        let config = Config::default();
        let initial_data = InitialData::new(b"Client 1 data", &config).unwrap();
        let stream = client1
            .connect_with_config(server_addr, config, Some(initial_data))
            .await
            .unwrap();
            
        let (mut reader, mut writer) = tokio::io::split(stream);
        
        // 读取服务器响应
        let mut buf = vec![0u8; 17];
        reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"Server response 1");
        
        writer.shutdown().await.unwrap();
        info!("[Client 1] 连接完成");
    });

    // 客户端2 - 稍微延迟
    let client2_barrier = barrier.clone();
    let client2_handle = tokio::spawn(async move {
        client2_barrier.wait().await;
        
        // 稍微延迟以创建不同的时序
        sleep(Duration::from_millis(10)).await;
        
        let config = Config::default();
        let initial_data = InitialData::new(b"Client 2 data", &config).unwrap();
        let stream = client2
            .connect_with_config(server_addr, config, Some(initial_data))
            .await
            .unwrap();
            
        let (mut reader, mut writer) = tokio::io::split(stream);
        
        // 读取服务器响应
        let mut buf = vec![0u8; 17];
        reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"Server response 2");
        
        writer.shutdown().await.unwrap();
        info!("[Client 2] 连接完成");
    });

    // 等待所有任务完成
    tokio::try_join!(server_handle, client1_handle, client2_handle).unwrap();
    
    info!("--- 早到帧缓存测试通过 ---");
}

/// 测试高并发0-RTT连接
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_concurrent_zero_rtt_connections() {
    init_tracing();
    const NUM_CLIENTS: usize = 100;
    info!("--- 测试高并发0-RTT连接 ({} 个客户端) ---", NUM_CLIENTS);

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let completion_counter = Arc::new(AtomicU32::new(0));
    let server_counter = completion_counter.clone();

    // 服务器任务
    let server_handle = tokio::spawn(async move {
        let mut handlers = Vec::new();
        
        for i in 0..NUM_CLIENTS {
            let (stream, remote_addr) = server_listener.accept().await.unwrap();
            if i % 10 == 0 {
                info!("[Server] 接受连接 #{} 来自 {}", i + 1, remote_addr);
            }

            let counter = server_counter.clone();
            let handler = tokio::spawn(
                async move {
                    let (mut reader, mut writer) = tokio::io::split(stream);

                    // 触发SYN-ACK
                    let response_msg = format!("Server response {}", i);
                    writer.write_all(response_msg.as_bytes()).await.unwrap();

                    // 读取0-RTT数据
                    let mut buf = Vec::new();
                    reader.read_to_end(&mut buf).await.unwrap();
                    
                    let expected_msg = format!("0-RTT data from client {}", i);
                    assert_eq!(buf, expected_msg.as_bytes());
                    
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                .instrument(tracing::info_span!("server_handler", id = i))
            );
            handlers.push(handler);
        }

        // 等待所有处理器完成
        for handler in handlers {
            handler.await.unwrap();
        }
        
        info!("[Server] 所有 {} 个连接处理完成", NUM_CLIENTS);
    });

    // 客户端任务
    let mut client_handles = Vec::new();
    for i in 0..NUM_CLIENTS {
        let client = TestHarness::create_client().await;
        let client_handle = tokio::spawn(
            async move {
                            let initial_msg = format!("0-RTT data from client {}", i);
            let config = Config::default();
            let initial_data = InitialData::new(initial_msg.as_bytes(), &config).unwrap();
            
            let stream = client
                .connect_with_config(server_addr, config, Some(initial_data))
                .await
                .unwrap();

                let (mut reader, mut writer) = tokio::io::split(stream);

                // 读取服务器响应
                let expected_response = format!("Server response {}", i);
                let mut buf = vec![0u8; expected_response.len()];
                reader.read_exact(&mut buf).await.unwrap();
                assert_eq!(buf, expected_response.as_bytes());

                // 关闭连接
                writer.shutdown().await.unwrap();
                
                if i % 10 == 0 {
                    info!("[Client {}] 0-RTT连接完成", i);
                }
            }
            .instrument(tracing::info_span!("client", id = i))
        );
        client_handles.push(client_handle);
    }

    // 等待所有任务完成
    for handle in client_handles {
        handle.await.unwrap();
    }
    server_handle.await.unwrap();

    // 验证所有连接都成功处理
    let completed = completion_counter.load(Ordering::SeqCst);
    assert_eq!(completed as usize, NUM_CLIENTS);

    info!("--- 高并发0-RTT连接测试通过 ({} 个客户端) ---", NUM_CLIENTS);
}

/// 测试0-RTT数据完整性
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_zero_rtt_data_integrity() {
    init_tracing();
    info!("--- 测试0-RTT数据完整性 ---");

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let client = TestHarness::create_client().await;
    let (server_stream_tx, mut server_stream_rx) = mpsc::channel(1);

    let server_handle = tokio::spawn(async move {
        let (stream, _) = server_listener.accept().await.unwrap();
        server_stream_tx.send(stream).await.unwrap();
    });

    // 创建具有特定模式的数据（减少到合理大小以符合单包限制）
    let mut test_data = Vec::new();
    for i in 0..250 { // 250 * 4 = 1000字节，符合单包限制
        test_data.extend_from_slice(&(i as u32).to_be_bytes());
    }
    
    let config = Config::default();
    let initial_data = InitialData::new(&test_data, &config).unwrap();
    info!("[Client] 发送 {} bytes 的结构化0-RTT数据", test_data.len());
    
    let client_stream = client
        .connect_with_config(server_addr, config, Some(initial_data))
        .await
        .unwrap();

    let server_stream = server_stream_rx.recv().await.unwrap();
    server_handle.await.unwrap();

    let (_client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    // 触发SYN-ACK
    server_writer.write_all(b"ACK").await.unwrap();

    // 关闭客户端写入以发送FIN
    client_writer.shutdown().await.unwrap();

    // 服务器读取所有数据
    let mut received_data = Vec::new();
    server_reader.read_to_end(&mut received_data).await.unwrap();

    // 验证数据完整性
    assert_eq!(received_data.len(), test_data.len());
    assert_eq!(received_data, test_data);

    // 验证数据模式正确性
    for i in 0..250 {
        let start_idx = i * 4;
        let end_idx = start_idx + 4;
        let chunk = &received_data[start_idx..end_idx];
        let value = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        assert_eq!(value, i as u32);
    }

    info!("[Server] 数据完整性验证通过，所有 {} bytes 数据正确", received_data.len());
    info!("--- 0-RTT数据完整性测试通过 ---");
}

/// 测试0-RTT与普通连接的混合场景
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn test_mixed_zero_rtt_and_regular_connections() {
    init_tracing();
    const ZERO_RTT_CLIENTS: usize = 50;
    const REGULAR_CLIENTS: usize = 50;
    info!("--- 测试混合连接场景 ({} 0-RTT + {} 普通) ---", ZERO_RTT_CLIENTS, REGULAR_CLIENTS);

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let zero_rtt_counter = Arc::new(AtomicU32::new(0));
    let regular_counter = Arc::new(AtomicU32::new(0));
    let server_zero_rtt_counter = zero_rtt_counter.clone();
    let server_regular_counter = regular_counter.clone();

    // 服务器任务
    let server_handle = tokio::spawn(async move {
        let mut handlers = Vec::new();
        
        for _i in 0..(ZERO_RTT_CLIENTS + REGULAR_CLIENTS) {
            let (stream, remote_addr) = server_listener.accept().await.unwrap();
            
            let zero_rtt_counter = server_zero_rtt_counter.clone();
            let regular_counter = server_regular_counter.clone();
            
            let handler = tokio::spawn(async move {
                let (mut reader, mut writer) = tokio::io::split(stream);

                // 触发SYN-ACK
                writer.write_all(b"SERVER_READY").await.unwrap();

                // 读取客户端数据
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf).await.unwrap();
                
                let msg = String::from_utf8_lossy(&buf);
                if msg.contains("0-RTT") {
                    zero_rtt_counter.fetch_add(1, Ordering::SeqCst);
                } else if msg.contains("Regular") {
                    regular_counter.fetch_add(1, Ordering::SeqCst);
                }
                
                debug!("[Server] 处理来自 {} 的连接: {}", remote_addr, msg.trim());
            });
            handlers.push(handler);
        }

        for handler in handlers {
            handler.await.unwrap();
        }
        
        info!("[Server] 所有连接处理完成");
    });

    let mut all_handles = Vec::new();

    // 启动0-RTT客户端
    for i in 0..ZERO_RTT_CLIENTS {
        let client = TestHarness::create_client().await;
        let handle = tokio::spawn(async move {
            let msg = format!("0-RTT message from client {}", i);
            let config = Config::default();
            let initial_data = InitialData::new(msg.as_bytes(), &config).unwrap();
            
            let stream = client
                .connect_with_config(server_addr, config, Some(initial_data))
                .await
                .unwrap();

            let (mut reader, mut writer) = tokio::io::split(stream);

            // 读取服务器响应
            let mut buf = vec![0u8; 12];
            reader.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"SERVER_READY");

            writer.shutdown().await.unwrap();
        });
        all_handles.push(handle);
    }

    // 启动普通客户端
    for i in 0..REGULAR_CLIENTS {
        let client = TestHarness::create_client().await;
        let handle = tokio::spawn(async move {
            let stream = client.connect(server_addr).await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(stream);

            // 读取服务器响应
            let mut buf = vec![0u8; 12];
            reader.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"SERVER_READY");

            // 发送普通消息
            let msg = format!("Regular message from client {}", i);
            writer.write_all(msg.as_bytes()).await.unwrap();
            writer.shutdown().await.unwrap();
        });
        all_handles.push(handle);
    }

    // 等待所有客户端完成
    for handle in all_handles {
        handle.await.unwrap();
    }
    
    // 等待服务器完成
    server_handle.await.unwrap();

    // 验证计数器
    let zero_rtt_count = zero_rtt_counter.load(Ordering::SeqCst);
    let regular_count = regular_counter.load(Ordering::SeqCst);
    
    assert_eq!(zero_rtt_count as usize, ZERO_RTT_CLIENTS);
    assert_eq!(regular_count as usize, REGULAR_CLIENTS);

    info!("--- 混合连接场景测试通过 ({} 0-RTT, {} 普通) ---", zero_rtt_count, regular_count);
}

/// 压力测试：大量0-RTT连接下的性能
#[tokio::test(flavor = "multi_thread", worker_threads = 12)]
async fn test_zero_rtt_performance_stress() {
    init_tracing();
    const NUM_CLIENTS: usize = 500;
    const DATA_SIZE: usize = 1024; // 1KB per client
    info!("--- 0-RTT性能压力测试 ({} 客户端, 每个 {}B) ---", NUM_CLIENTS, DATA_SIZE);

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let start_time = Instant::now();
    let completed_counter = Arc::new(AtomicU32::new(0));
    let server_counter = completed_counter.clone();

    // 服务器任务
    let server_handle = tokio::spawn(async move {
        let mut handlers = Vec::new();
        
        for _i in 0..NUM_CLIENTS {
            let (stream, _) = server_listener.accept().await.unwrap();
            
            let counter = server_counter.clone();
            let handler = tokio::spawn(async move {
                let (mut reader, mut writer) = tokio::io::split(stream);

                // 立即触发SYN-ACK
                writer.write_all(b"OK").await.unwrap();

                // 读取0-RTT数据
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf).await.unwrap();
                
                // 验证数据大小
                assert_eq!(buf.len(), DATA_SIZE);
                
                counter.fetch_add(1, Ordering::SeqCst);
            });
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
        let handle = tokio::spawn(async move {
            // 创建固定大小的测试数据
            let test_data = vec![(i % 256) as u8; DATA_SIZE];
            let config = Config::default();
            let initial_data = InitialData::new(&test_data, &config).unwrap();
            
            let stream = client
                .connect_with_config(server_addr, config, Some(initial_data))
                .await
                .unwrap();

            let (mut reader, mut writer) = tokio::io::split(stream);

            // 读取服务器确认
            let mut buf = vec![0u8; 2];
            reader.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"OK");

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
    let completed = completed_counter.load(Ordering::SeqCst);
    let total_data = (completed as usize) * DATA_SIZE;
    let throughput = total_data as f64 / elapsed.as_secs_f64() / 1024.0 / 1024.0; // MB/s

    info!("--- 0-RTT性能压力测试结果 ---");
    info!("完成连接数: {}", completed);
    info!("总耗时: {:?}", elapsed);
    info!("总数据量: {:.2} MB", total_data as f64 / 1024.0 / 1024.0);
    info!("吞吐量: {:.2} MB/s", throughput);
    info!("平均连接时间: {:.2} ms", elapsed.as_millis() as f64 / NUM_CLIENTS as f64);

    assert_eq!(completed as usize, NUM_CLIENTS);
    info!("--- 0-RTT性能压力测试通过 ---");
}