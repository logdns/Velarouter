//! Exercise both ends of the transfer protocol without a desktop or network.
use super::*;
use vr_agent::{
    files::FileService,
    transfer::{FileOut, TransferManager},
};
use vr_proto::Control;

#[tokio::test]
async fn selected_folders_roundtrip_including_empty_files() {
    let temp = std::env::temp_dir().join(format!(
        "vr-transfer-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let remote = FileService::new(temp.join("remote")).unwrap();
    std::fs::create_dir(remote.root().join("chosen")).unwrap();
    let local = temp.join("downloads");
    std::fs::create_dir(&local).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let mut agent = TransferManager::new(tx);
    for (index, data) in [Vec::new(), vec![21u8; CHUNK + 33]].into_iter().enumerate() {
        let name = format!("file-{index}.bin");
        let path = format!("chosen/{name}");
        let source = temp.join(&name);
        std::fs::write(&source, &data).unwrap();
        let mut upload = Upload::start(1, source).await.unwrap();
        let start = agent
            .handle_control(
                Control::FilePush {
                    transfer_id: 1,
                    name: path.clone(),
                    size: upload.total,
                    offset: 0,
                    overwrite: false,
                },
                &remote,
            )
            .await;
        assert!(matches!(start.as_slice(), [Control::FileProgress { .. }]));
        while !upload.finished_sending() {
            let (header, payload, _) = upload.next_chunk().await.unwrap().unwrap();
            let replies = agent.handle_chunk(header, &payload).await;
            assert_eq!(
                replies
                    .iter()
                    .any(|r| matches!(r, Control::FileComplete { transfer_id: 1, .. })),
                upload.finished_sending()
            );
        }
        assert!(!remote.root().join(&name).exists());
        let listing = agent
            .handle_control(
                Control::FileListRequest {
                    request_id: 7,
                    path: "chosen".into(),
                },
                &remote,
            )
            .await;
        assert!(
            matches!(&listing[..], [Control::FileList {request_id:7,path:dir,root:Some(root),entries}] if dir == "chosen" && Path::new(root) == remote.root() && entries.iter().any(|f|f.path == path))
        );
        let mut download = Download::start(2, &path, Some(&local)).await.unwrap();
        agent
            .handle_control(
                Control::FilePull {
                    transfer_id: 2,
                    path,
                    offset: 0,
                },
                &remote,
            )
            .await;
        loop {
            match rx.recv().await.unwrap() {
                FileOut::Chunk { data, .. } => {
                    download.write(&data).await.unwrap();
                }
                FileOut::Done { sha256, .. } => {
                    assert!(download.finish(&sha256).await);
                    break;
                }
                reply => panic!("unexpected reply: {reply:?}"),
            }
        }
        assert_eq!(std::fs::read(local.join(&name)).unwrap(), data);
    }
    assert!(Download::start(3, "file.bin", Some(&temp.join("missing")))
        .await
        .is_err());
    std::fs::remove_dir_all(temp).unwrap();
}
