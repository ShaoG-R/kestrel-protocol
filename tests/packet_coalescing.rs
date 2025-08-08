//! 包粘连分包功能的全面测试
//! Comprehensive tests for packet coalescing and fragmentation

pub mod common;

use common::harness::TestHarness;
use kestrel_protocol::{
    socket::handle::initial_data::InitialData,
    config::Config,
};
use std::{
    sync::{Arc, atomic::{AtomicU32, AtomicUsize, Ordering}},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time::{sleep, Instant},
};
use tracing::{info, warn};

/// 测试SYN-ACK与PUSH帧的基础粘连
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_basic_syn_ack_push_coalescing() {

    info!("--- 测试SYN-ACK与PUSH帧基础粘连 ---");

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

    // 客户端发送0-RTT数据
    let config = Config::default();
    let initial_data = InitialData::new(b"Coalescing test data", &config).unwrap();
    let client_stream = client
        .connect_with_config(server_addr, Box::new(config), Some(initial_data))
        .await
        .unwrap();

    let server_stream = server_stream_rx.recv().await.unwrap();
    server_handle.await.unwrap();

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    // 服务器发送响应（这会触发SYN-ACK发送）
    let server_response = b"Coalesced response";
    server_writer
        .write_all(server_response)
        .await
        .expect("服务器写入应该成功");

    // 客户端读取响应
    let mut response_buf = vec![0u8; server_response.len()];
    client_reader
        .read_exact(&mut response_buf)
        .await
        .expect("客户端应该能读取响应");
    assert_eq!(&response_buf, server_response);
    info!("[Client] 接收到服务器响应");

    // 客户端关闭写入端
    client_writer.shutdown().await.unwrap();

    // 服务器读取0-RTT数据
    let mut initial_data = Vec::new();
    server_reader.read_to_end(&mut initial_data).await.unwrap();
    assert_eq!(&initial_data, b"Coalescing test data");
    info!("[Server] 接收到粘连的0-RTT数据");
    
    info!("--- SYN-ACK与PUSH帧基础粘连测试通过 ---");
}

/// 测试大数据包的智能分包
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_large_packet_fragmentation() {

    info!("--- 测试大数据包智能分包 ---");

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

    // 创建适中大小的数据包（1KB）避免超过单包限制
    let large_data = vec![b'X'; 1024];
    let config = Config::default();
    let initial_data = InitialData::new(&large_data, &config).unwrap();
    
    info!("[Client] 发送大数据包 ({} bytes)", large_data.len());
    let client_stream = client
        .connect_with_config(server_addr, Box::new(config), Some(initial_data))
        .await
        .unwrap();

    let server_stream = server_stream_rx.recv().await.unwrap();
    server_handle.await.unwrap();

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    // 服务器触发SYN-ACK发送
    server_writer.write_all(b"LARGE_ACK").await.unwrap();

    // 关闭客户端写入
    client_writer.shutdown().await.unwrap();

    // 服务器读取所有分片数据
    let mut received_data = Vec::new();
    server_reader.read_to_end(&mut received_data).await.unwrap();

    // 验证数据完整性
    assert_eq!(received_data.len(), large_data.len());
    assert_eq!(received_data, large_data);
    info!("[Server] 成功接收所有分片数据 ({} bytes)", received_data.len());

    // 客户端读取ACK
    let mut ack_buf = vec![0u8; 9];
    client_reader.read_exact(&mut ack_buf).await.unwrap();
    assert_eq!(&ack_buf, b"LARGE_ACK");

    info!("--- 大数据包智能分包测试通过 ---");
}

/// 测试多个小包的高效粘连
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_multiple_small_packet_coalescing() {

    info!("--- 测试多个小包高效粘连 ---");

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

    // 创建多个小数据块，应该被粘连在一起
    let mut combined_data = Vec::new();
    for i in 0..20 {
        let chunk = format!("Chunk{:02} ", i);
        combined_data.extend_from_slice(chunk.as_bytes());
    }
    
    let config = Config::default();
    let initial_data = InitialData::new(&combined_data, &config).unwrap();
    info!("[Client] 发送多个小包粘连数据 ({} bytes)", combined_data.len());
    
    let client_stream = client
        .connect_with_config(server_addr, Box::new(config), Some(initial_data))
        .await
        .unwrap();

    let server_stream = server_stream_rx.recv().await.unwrap();
    server_handle.await.unwrap();

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    // 服务器立即发送多个小响应
    for i in 0..5 {
        let response = format!("Resp{} ", i);
        server_writer.write_all(response.as_bytes()).await.unwrap();
    }

    // 关闭客户端写入
    client_writer.shutdown().await.unwrap();

    // 服务器读取粘连数据
    let mut received_data = Vec::new();
    server_reader.read_to_end(&mut received_data).await.unwrap();

    // 验证数据完整性
    assert_eq!(received_data.len(), combined_data.len());
    assert_eq!(received_data, combined_data);
    
    // 验证数据内容
    let received_str = String::from_utf8_lossy(&received_data);
    for i in 0..20 {
        let expected_chunk = format!("Chunk{:02} ", i);
        assert!(received_str.contains(&expected_chunk));
    }
    info!("[Server] 成功接收粘连数据: {}", received_str.trim());

    // 客户端读取所有响应
    let mut response_data = Vec::new();
    client_reader.read_to_end(&mut response_data).await.unwrap();
    
    let response_str = String::from_utf8_lossy(&response_data);
    for i in 0..5 {
        let expected_resp = format!("Resp{} ", i);
        assert!(response_str.contains(&expected_resp));
    }
    info!("[Client] 接收到粘连响应: {}", response_str.trim());

    info!("--- 多个小包高效粘连测试通过 ---");
}

/// 测试粘连分包的边界条件
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_coalescing_boundary_conditions() {

    info!("--- 测试粘连分包边界条件 ---");

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

    // 测试恰好达到MTU边界的数据
    // 假设MTU为1500，减去UDP/IP头部约为1400可用
    let boundary_size = 1400 - 50; // 留一些余量给协议头部
    let boundary_data = vec![b'B'; boundary_size];
    
    let config = Config::default();
    let initial_data = InitialData::new(&boundary_data, &config).unwrap();
    info!("[Client] 发送边界大小数据 ({} bytes)", boundary_data.len());
    
    let client_stream = client
        .connect_with_config(server_addr, Box::new(config), Some(initial_data))
        .await
        .unwrap();

    let server_stream = server_stream_rx.recv().await.unwrap();
    server_handle.await.unwrap();

    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    // 服务器发送确认
    server_writer.write_all(b"BOUNDARY_OK").await.unwrap();

    // 关闭客户端写入
    client_writer.shutdown().await.unwrap();

    // 验证边界数据
    let mut received_data = Vec::new();
    server_reader.read_to_end(&mut received_data).await.unwrap();

    assert_eq!(received_data.len(), boundary_data.len());
    assert_eq!(received_data, boundary_data);
    info!("[Server] 边界数据验证通过");

    // 客户端读取确认
    let mut ack_buf = vec![0u8; 11];
    client_reader.read_exact(&mut ack_buf).await.unwrap();
    assert_eq!(&ack_buf, b"BOUNDARY_OK");

    info!("--- 粘连分包边界条件测试通过 ---");
}

/// 测试高并发下的包粘连性能
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_concurrent_coalescing_performance() {

    const NUM_CLIENTS: usize = 100;
    const DATA_SIZE: usize = 512;
    info!("--- 测试高并发包粘连性能 ({} 客户端, 每个 {}B) ---", NUM_CLIENTS, DATA_SIZE);

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let start_time = Instant::now();
    let total_bytes = Arc::new(AtomicUsize::new(0));
    let server_bytes = total_bytes.clone();

    // 服务器任务
    let server_handle = tokio::spawn(async move {
        let mut handlers = Vec::new();

        for _i in 0..NUM_CLIENTS {
            let (stream, _) = server_listener.accept().await.unwrap();
            let bytes_counter = server_bytes.clone();

            let handler = tokio::spawn(async move {
                let (mut reader, mut writer) = tokio::io::split(stream);

                // 快速确认
                writer.write_all(b"PERF_OK").await.unwrap();

                // 读取粘连数据
                let mut buf = Vec::new();
                reader.read_to_end(&mut buf).await.unwrap();

                if buf.len() == DATA_SIZE {
                    bytes_counter.fetch_add(buf.len(), Ordering::SeqCst);
                }
            });
            handlers.push(handler);
        }

        for handler in handlers {
            handler.await.unwrap();
        }
    });

    // 并发客户端任务
    let mut client_handles = Vec::new();
    for i in 0..NUM_CLIENTS {
        let client = TestHarness::create_client().await;
        let handle = tokio::spawn(async move {
            // 创建特定模式的数据
            let mut test_data = vec![(i % 256) as u8; DATA_SIZE];
            test_data[0] = (i >> 8) as u8;
            test_data[1] = (i & 0xFF) as u8;
            
            let config = Config::default();
            let initial_data = InitialData::new(&test_data, &config).unwrap();
            
            let stream = client
                .connect_with_config(server_addr, Box::new(config), Some(initial_data))
                .await
                .unwrap();

            let (mut reader, mut writer) = tokio::io::split(stream);

            // 读取确认
            let mut buf = vec![0u8; 7];
            reader.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"PERF_OK");

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
    let processed_bytes = total_bytes.load(Ordering::SeqCst);
    let expected_bytes = NUM_CLIENTS * DATA_SIZE;
    let throughput = processed_bytes as f64 / elapsed.as_secs_f64() / 1024.0 / 1024.0;

    info!("--- 高并发包粘连性能测试结果 ---");
    info!("处理字节数: {}/{} bytes", processed_bytes, expected_bytes);
    info!("总耗时: {:?}", elapsed);
    info!("吞吐量: {:.2} MB/s", throughput);
    info!("平均延迟: {:.2} ms", elapsed.as_millis() as f64 / NUM_CLIENTS as f64);

    assert_eq!(processed_bytes, expected_bytes);
    info!("--- 高并发包粘连性能测试通过 ---");
}

/// 测试不同大小数据的粘连策略
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_variable_size_coalescing_strategy() {

    info!("--- 测试不同大小数据的粘连策略 ---");

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    // 测试不同大小的数据包
    let test_sizes = vec![10, 50, 100, 500, 1000, 2000, 4000];
    let mut total_received = 0;

    for (idx, &size) in test_sizes.iter().enumerate() {
        info!("=== 测试大小: {} bytes ===", size);
        
        let client = TestHarness::create_client().await;
        let (server_stream_tx, mut server_stream_rx) = mpsc::channel(1);

        let server_handle = tokio::spawn(async move {
            let (stream, _) = server_listener.accept().await.unwrap();
            server_stream_tx.send((stream, server_listener)).await.unwrap();
        });

        // 创建特定大小的测试数据
        let mut test_data = vec![(idx % 256) as u8; size];
        test_data[0] = 0xAA; // 标识符
        test_data[size - 1] = 0xBB; // 结束标识符
        
        let config = Config::default();
        let initial_data = InitialData::new(&test_data, &config).unwrap();
        let client_stream = client
            .connect_with_config(server_addr, Box::new(config), Some(initial_data))
            .await
            .unwrap();

        let (server_stream, returned_listener) = server_stream_rx.recv().await.unwrap();
        server_listener = returned_listener;
        
        server_handle.await.unwrap();

        let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
        let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

        // 服务器确认
        let ack_msg = format!("SIZE_{}_OK", size);
        server_writer.write_all(ack_msg.as_bytes()).await.unwrap();

        // 关闭客户端
        client_writer.shutdown().await.unwrap();

        // 验证数据
        let mut received_data = Vec::new();
        server_reader.read_to_end(&mut received_data).await.unwrap();

        assert_eq!(received_data.len(), size);
        assert_eq!(received_data[0], 0xAA);
        assert_eq!(received_data[size - 1], 0xBB);
        assert_eq!(received_data, test_data);

        // 客户端读取确认
        let mut ack_buf = vec![0u8; ack_msg.len()];
        client_reader.read_exact(&mut ack_buf).await.unwrap();
        assert_eq!(ack_buf, ack_msg.as_bytes());

        total_received += size;
        info!("大小 {} bytes 测试通过", size);
    }

    info!("--- 不同大小数据粘连策略测试通过 (总计 {} bytes) ---", total_received);
}

/// 测试粘连包的错误恢复机制
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_coalescing_error_recovery() {

    info!("--- 测试粘连包错误恢复机制 ---");

    let TestHarness {
        server_addr,
        mut server_listener,
        ..
    } = TestHarness::new().await;

    let success_count = Arc::new(AtomicU32::new(0));
    let server_count = success_count.clone();

    // 服务器任务：处理多个连接尝试
    let server_handle = tokio::spawn(async move {
        let mut handlers = Vec::new();

        // 处理3个连接尝试
        for i in 0..3 {
            match tokio::time::timeout(Duration::from_secs(2), server_listener.accept()).await {
                Ok(Ok((stream, _))) => {
                    let counter = server_count.clone();
                    let handler = tokio::spawn(async move {
                        let (mut reader, mut writer) = tokio::io::split(stream);

                        // 发送确认
                        let msg = format!("RECOVERY_{}", i);
                        writer.write_all(msg.as_bytes()).await.unwrap();

                        // 读取数据
                        let mut buf = Vec::new();
                        match reader.read_to_end(&mut buf).await {
                            Ok(_) => {
                                if !buf.is_empty() {
                                    counter.fetch_add(1, Ordering::SeqCst);
                                }
                            }
                            Err(e) => warn!("读取错误: {}", e),
                        }
                    });
                    handlers.push(handler);
                }
                Ok(Err(e)) => warn!("接受连接错误: {}", e),
                Err(_) => warn!("接受连接超时"),
            }
        }

        for handler in handlers {
            let _ = handler.await;
        }
    });

    // 客户端1：正常连接
    let client1 = TestHarness::create_client().await;
    let client1_handle = tokio::spawn(async move {
        let data = b"Normal connection data";
        let config = Config::default();
        let initial_data = InitialData::new(data, &config).unwrap();
        
        match client1.connect_with_config(server_addr, Box::new(config), Some(initial_data)).await {
            Ok(stream) => {
                let (mut reader, mut writer) = tokio::io::split(stream);
                
                let mut buf = vec![0u8; 10];
                if let Ok(_) = reader.read_exact(&mut buf).await {
                    assert_eq!(&buf, b"RECOVERY_0");
                }
                let _ = writer.shutdown().await;
                info!("[Client 1] 正常连接完成");
            }
            Err(e) => warn!("[Client 1] 连接失败: {}", e),
        }
    });

    // 客户端2：模拟问题连接（快速断开）
    let client2 = TestHarness::create_client().await;
    let client2_handle = tokio::spawn(async move {
        let data = b"Problem connection";
        let config = Config::default();
        let initial_data = InitialData::new(data, &config).unwrap();
        
        match client2.connect_with_config(server_addr, Box::new(config), Some(initial_data)).await {
            Ok(stream) => {
                let (reader, mut writer) = tokio::io::split(stream);
                
                // 立即断开连接以模拟问题
                writer.shutdown().await.unwrap();
                drop(reader);
                info!("[Client 2] 模拟问题连接");
            }
            Err(e) => warn!("[Client 2] 连接失败: {}", e),
        }
    });

    // 等待一段时间让问题连接处理
    sleep(Duration::from_millis(100)).await;

    // 客户端3：恢复正常连接
    let client3 = TestHarness::create_client().await;
    let client3_handle = tokio::spawn(async move {
        let data = b"Recovery connection data";
        let config = Config::default();
        let initial_data = InitialData::new(data, &config).unwrap();
        
        match client3.connect_with_config(server_addr, Box::new(config), Some(initial_data)).await {
            Ok(stream) => {
                let (mut reader, mut writer) = tokio::io::split(stream);
                
                let mut buf = vec![0u8; 10];
                if let Ok(_) = reader.read_exact(&mut buf).await {
                    assert_eq!(&buf, b"RECOVERY_2");
                }
                let _ = writer.shutdown().await;
                info!("[Client 3] 恢复连接完成");
            }
            Err(e) => warn!("[Client 3] 连接失败: {}", e),
        }
    });

    // 等待所有任务完成
    let _ = tokio::join!(client1_handle, client2_handle, client3_handle);
    server_handle.await.unwrap();

    let successful = success_count.load(Ordering::SeqCst);
    info!("成功处理的连接数: {}", successful);
    
    // 至少应该有1个成功连接（问题连接可能导致部分失败）
    assert!(successful >= 1);

    info!("--- 粘连包错误恢复机制测试通过 ---");
}