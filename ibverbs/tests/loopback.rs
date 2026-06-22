//! Integration tests that exercise the RDMA data path end to end.
//!
//! Each test self-loops a queue pair: the queue pair is connected to its own endpoint, so messages
//! it sends are delivered back to itself. This needs only one host and no peer, which makes the data
//! path testable with Soft-RoCE (rxe).
//!
//! These tests are opt-in / local-only: they run only when the `IBVERBS_TEST_DEVICE` environment
//! variable names an RDMA device to use (for example `IBVERBS_TEST_DEVICE=rxe0`). When it is unset
//! every test prints a notice and returns, so `cargo test` stays green on machines without RDMA and
//! on GitHub CI (whose hosted runners cannot load rxe). Selecting the device by name (rather than
//! taking the first one) matters because some environments expose an unrelated, unusable RDMA device
//! that must not be picked up by accident. See `configure-softroce.sh` for local setup.

use std::time::{Duration, Instant};

use ibverbs::{
    ibv_access_flags, ibv_qp_type, CompletionQueue, Context, ProtectionDomain, QueuePair,
    WorkRequest,
};

/// A queue pair connected to itself, together with the resources that must outlive it.
struct Loopback {
    // Fields drop in declaration order, and ibverbs requires destroying the queue pair before its
    // completion queue (and both before the protection domain and context), so declare them in that
    // order to avoid an EBUSY panic in `Drop`.
    qp: QueuePair,
    cq: CompletionQueue,
    pd: ProtectionDomain,
    _ctx: Context,
}

/// Open the device named by `IBVERBS_TEST_DEVICE`, or `None` (skip) if that variable is unset.
///
/// Panics if the variable names a device that is not present, so a misconfigured run fails loudly
/// rather than silently skipping.
fn open_test_device() -> Option<Context> {
    let want = match std::env::var("IBVERBS_TEST_DEVICE") {
        Ok(name) if !name.is_empty() => name,
        _ => return None,
    };
    let devices = ibverbs::devices().expect("failed to list RDMA devices");
    let device = devices
        .iter()
        .find(|d| d.name().is_some_and(|n| n.to_bytes() == want.as_bytes()))
        .unwrap_or_else(|| {
            panic!("IBVERBS_TEST_DEVICE={want} is not among the available RDMA devices")
        });
    Some(device.open().expect("failed to open the RDMA device"))
}

/// Build a self-connected queue pair of the given type on the configured device, or `None` if
/// testing is disabled. RC/UC get generous queue and scatter-gather limits and (for RC/UC) remote
/// access, so the tests below can post batches, multi-SGE work requests, and one-sided operations.
fn loopback_of(qp_type: ibv_qp_type) -> Option<Loopback> {
    let ctx = open_test_device()?;
    let cq = ctx
        .create_cq(64, 0)
        .expect("failed to create completion queue");
    let pd = ctx
        .alloc_pd()
        .expect("failed to allocate protection domain");

    let mut builder = pd
        .create_qp(&cq, &cq, qp_type)
        .expect("failed to create queue pair");
    builder
        .set_gid_index(1)
        .set_max_send_wr(16)
        .set_max_recv_wr(16)
        .set_max_send_sge(4)
        .set_max_recv_sge(4);
    // Self-loopback one-sided ops target this same QP, so it must grant remote access. RC also
    // needs remote-atomic access for the atomic test; allow_remote_rw covers UC (and is a no-op for
    // UD).
    if qp_type == ibv_qp_type::IBV_QPT_RC {
        builder.set_access(
            ibv_access_flags::IBV_ACCESS_LOCAL_WRITE
                | ibv_access_flags::IBV_ACCESS_REMOTE_WRITE
                | ibv_access_flags::IBV_ACCESS_REMOTE_READ
                | ibv_access_flags::IBV_ACCESS_REMOTE_ATOMIC,
        );
    } else {
        builder.allow_remote_rw();
    }

    let prepared = builder.build().expect("failed to build queue pair");
    let endpoint = prepared.endpoint().expect("failed to read local endpoint");
    let qp = prepared
        .handshake(endpoint)
        .expect("failed to transition queue pair to RTS");

    Some(Loopback {
        qp,
        cq,
        pd,
        _ctx: ctx,
    })
}

/// A reliable-connected self-loopback queue pair (the common case).
fn loopback() -> Option<Loopback> {
    loopback_of(ibv_qp_type::IBV_QPT_RC)
}

/// Poll `cq` until at least `n` completions have arrived, asserting each completed successfully, and
/// return copies of every completion observed. Panics if `n` completions do not arrive within a few
/// seconds (Soft-RoCE loopback completes in microseconds).
fn drain(cq: &CompletionQueue, n: usize) -> Vec<ibverbs::ibv_wc> {
    let mut observed = Vec::with_capacity(n);
    let mut completions = [ibverbs::ibv_wc::default(); 16];
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        let done = cq.poll(&mut completions).expect("failed to poll CQ");
        for wc in done.iter() {
            if let Some((status, vendor_err)) = wc.error() {
                panic!(
                    "work request {} failed: status {status:?}, vendor_err {vendor_err}",
                    wc.wr_id()
                );
            }
            observed.push(*wc);
        }
        if observed.len() >= n {
            return observed;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for completions: got {} of {n}",
            observed.len()
        );
    }
}

/// Read the first 8 bytes of a buffer as a native-endian `u64` (for inspecting atomic results).
fn first_u64(bytes: &[u8]) -> u64 {
    u64::from_ne_bytes(bytes[..8].try_into().unwrap())
}

/// Print a skip notice and return when `IBVERBS_TEST_DEVICE` is not set.
macro_rules! require_device {
    ($test:literal) => {
        if std::env::var_os("IBVERBS_TEST_DEVICE").is_none() {
            eprintln!(concat!(
                "skipping ",
                $test,
                ": set IBVERBS_TEST_DEVICE=<rdma device> (e.g. rxe0) to run"
            ));
            return;
        }
    };
}

/// Reading the GID table of the configured device exercises the control path.
#[test]
fn gid_table() {
    require_device!("gid_table");
    let ctx = open_test_device().expect("device requested but not opened");
    let gids = ctx.gid_table().expect("failed to read GID table");
    assert!(!gids.is_empty(), "expected at least one GID entry");
}

/// Two-sided SEND / RECV: a posted receive catches a send to the same queue pair.
#[test]
fn send_recv() {
    require_device!("send_recv");
    let mut lb = loopback().expect("device requested but loopback not set up");

    let mut recv = lb.pd.allocate(64).expect("failed to register recv MR");
    let mut send = lb.pd.allocate(64).expect("failed to register send MR");
    send.inner_mut()[..5].copy_from_slice(b"hello");

    unsafe { lb.qp.post_receive(&[recv.slice(..5)], 1) }.expect("post_receive failed");
    unsafe { lb.qp.post_send(&[send.slice(..5)], 2) }.expect("post_send failed");

    let comps = drain(&lb.cq, 2);
    assert!(
        comps.iter().any(|wc| wc.wr_id() == 1),
        "missing recv completion"
    );
    assert!(
        comps.iter().any(|wc| wc.wr_id() == 2),
        "missing send completion"
    );
    assert_eq!(&recv.inner_mut()[..5], b"hello");
}

/// A larger transfer that spans multiple MTU-sized packets reports the right received byte length.
#[test]
fn send_recv_large() {
    require_device!("send_recv_large");
    let mut lb = loopback().expect("device requested but loopback not set up");

    const LEN: usize = 4096;
    let mut recv = lb.pd.allocate(LEN).expect("failed to register recv MR");
    let mut send = lb.pd.allocate(LEN).expect("failed to register send MR");
    for (i, b) in send.inner_mut().iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }

    unsafe { lb.qp.post_receive(&[recv.slice(..LEN)], 1) }.expect("post_receive failed");
    unsafe { lb.qp.post_send(&[send.slice(..LEN)], 2) }.expect("post_send failed");

    let comps = drain(&lb.cq, 2);
    let recv_wc = comps
        .iter()
        .find(|wc| wc.wr_id() == 1)
        .expect("missing recv completion");
    assert_eq!(recv_wc.len(), LEN, "received byte length mismatch");
    assert_eq!(recv.inner_mut().as_slice(), send.inner_mut().as_slice());
}

/// Scatter-gather: a send gathers two non-contiguous source slices, and the receive scatters the
/// resulting contiguous message into two non-contiguous destination slices.
#[test]
fn scatter_gather() {
    require_device!("scatter_gather");
    let mut lb = loopback().expect("device requested but loopback not set up");

    let mut send = lb.pd.allocate(64).expect("failed to register send MR");
    let mut recv = lb.pd.allocate(64).expect("failed to register recv MR");
    send.inner_mut()[0..4].copy_from_slice(b"AAAA");
    send.inner_mut()[16..20].copy_from_slice(b"BBBB");

    unsafe {
        lb.qp
            .post_receive(&[recv.slice(0..4), recv.slice(32..36)], 1)
    }
    .expect("post_receive failed");
    unsafe { lb.qp.post_send(&[send.slice(0..4), send.slice(16..20)], 2) }
        .expect("post_send failed");

    let comps = drain(&lb.cq, 2);
    let recv_wc = comps
        .iter()
        .find(|wc| wc.wr_id() == 1)
        .expect("missing recv completion");
    assert_eq!(recv_wc.len(), 8, "scattered byte length mismatch");
    // The 8-byte gathered message "AAAABBBB" is scattered into the two receive slices in order.
    assert_eq!(&recv.inner_mut()[0..4], b"AAAA");
    assert_eq!(&recv.inner_mut()[32..36], b"BBBB");
}

/// One-sided RDMA WRITE: the initiator writes directly into a remote memory region.
#[test]
fn rdma_write() {
    require_device!("rdma_write");
    let mut lb = loopback().expect("device requested but loopback not set up");

    let mut src = lb.pd.allocate(64).expect("failed to register src MR");
    let mut dst = lb.pd.allocate(64).expect("failed to register dst MR");
    src.inner_mut()[..6].copy_from_slice(b"verbs!");

    let remote = dst.remote().slice(..6);
    unsafe { lb.qp.post_write(&[src.slice(..6)], remote, 1, None) }.expect("post_write failed");

    let comps = drain(&lb.cq, 1);
    assert_eq!(comps[0].wr_id(), 1);
    assert_eq!(&dst.inner_mut()[..6], b"verbs!");
}

/// RDMA WRITE with immediate: the write lands in remote memory and also consumes a receive work
/// request, whose completion carries the immediate value.
#[test]
fn rdma_write_with_imm() {
    require_device!("rdma_write_with_imm");
    let mut lb = loopback().expect("device requested but loopback not set up");

    let mut src = lb.pd.allocate(64).expect("failed to register src MR");
    let mut dst = lb.pd.allocate(64).expect("failed to register dst MR");
    let dummy = lb.pd.allocate(64).expect("failed to register dummy MR");
    src.inner_mut()[..4].copy_from_slice(&[1, 2, 3, 4]);

    // A write-with-immediate consumes a receive work request on the target queue pair.
    unsafe { lb.qp.post_receive(&[dummy.slice(..1)], 10) }.expect("post_receive failed");

    let imm = 0xdead_beef_u32;
    let remote = dst.remote().slice(..4);
    unsafe { lb.qp.post_write(&[src.slice(..4)], remote, 11, Some(imm)) }
        .expect("post_write failed");

    let comps = drain(&lb.cq, 2);
    assert_eq!(&dst.inner_mut()[..4], &[1, 2, 3, 4]);
    let recv = comps
        .iter()
        .find(|wc| wc.wr_id() == 10)
        .expect("missing recv completion for write-with-imm");
    // `post_write` puts the immediate on the wire in network byte order and `imm_data()` returns it
    // raw, so decode it before comparing.
    assert_eq!(
        recv.imm_data().map(u32::from_be),
        Some(imm),
        "immediate value not delivered"
    );
}

/// One-sided RDMA READ: the initiator reads directly from a remote memory region.
#[test]
fn rdma_read() {
    require_device!("rdma_read");
    let mut lb = loopback().expect("device requested but loopback not set up");

    let mut remote_mr = lb.pd.allocate(64).expect("failed to register remote MR");
    let mut local = lb.pd.allocate(64).expect("failed to register local MR");
    remote_mr.inner_mut()[..8].copy_from_slice(&[9, 8, 7, 6, 5, 4, 3, 2]);

    let remote = remote_mr.remote().slice(..8);
    unsafe { lb.qp.post_read(&[local.slice(..8)], remote, 1) }.expect("post_read failed");

    let comps = drain(&lb.cq, 1);
    assert_eq!(comps[0].wr_id(), 1);
    assert_eq!(&local.inner_mut()[..8], &[9, 8, 7, 6, 5, 4, 3, 2]);
}

/// Atomic compare-and-swap and fetch-and-add against a remote 8-byte value.
///
/// RDMA atomics operate on 8-byte, 8-byte-aligned values, and their wire byte order is
/// implementation-defined, so the assertions are written to be byte-order agnostic (compared
/// against zero, or accepting either endianness of a stored value).
#[test]
fn atomic_operations() {
    require_device!("atomic_operations");
    let mut lb = loopback().expect("device requested but loopback not set up");

    let mut target = lb.pd.allocate(8).expect("failed to register target MR");
    let mut local = lb.pd.allocate(8).expect("failed to register local MR");

    // Compare-and-swap on a zeroed target: 0 == 0 in any byte order, so the swap succeeds and the
    // original value (0) is returned into `local`.
    let swapped = 0x1122_3344_5566_7788_u64;
    let remote = target.remote().slice(..8);
    unsafe {
        lb.qp
            .post_atomic_cmp_swap(&[local.slice(..8)], remote, 0, swapped, 1)
    }
    .expect("post_atomic_cmp_swap failed");
    assert_eq!(drain(&lb.cq, 1)[0].wr_id(), 1);
    assert_eq!(
        first_u64(local.inner_mut()),
        0,
        "CAS must return the original value"
    );
    let stored = first_u64(target.inner_mut());
    assert!(
        stored == swapped || stored == swapped.swap_bytes(),
        "CAS must store the swap value (in some byte order)"
    );

    // A mismatching compare leaves the target unchanged and returns its current value.
    let remote = target.remote().slice(..8);
    unsafe {
        lb.qp
            .post_atomic_cmp_swap(&[local.slice(..8)], remote, 0, 0, 2)
    }
    .expect("post_atomic_cmp_swap failed");
    assert_eq!(drain(&lb.cq, 1)[0].wr_id(), 2);
    assert_eq!(
        first_u64(target.inner_mut()),
        stored,
        "mismatching CAS must not modify the target"
    );
    assert_eq!(
        first_u64(local.inner_mut()),
        stored,
        "mismatching CAS returns the current value"
    );

    // Fetch-and-add on a fresh zeroed counter returns the original (0) and adds.
    let mut counter = lb.pd.allocate(8).expect("failed to register counter MR");
    let remote = counter.remote().slice(..8);
    unsafe {
        lb.qp
            .post_atomic_fetch_add(&[local.slice(..8)], remote, 5, 3)
    }
    .expect("post_atomic_fetch_add failed");
    assert_eq!(drain(&lb.cq, 1)[0].wr_id(), 3);
    assert_eq!(
        first_u64(local.inner_mut()),
        0,
        "fetch-add must return the original value"
    );
    let sum = first_u64(counter.inner_mut());
    assert!(sum == 5 || sum == 5u64.swap_bytes(), "fetch-add must add 5");
}

/// Batched posting: a single `post` call chains an (unsignaled) RDMA write followed by a signaled
/// send. Only the send is signaled, so the send queue yields exactly one completion.
#[test]
fn batched_post() {
    require_device!("batched_post");
    let mut lb = loopback().expect("device requested but loopback not set up");

    let mut payload = lb.pd.allocate(64).expect("failed to register payload MR");
    let mut dst = lb.pd.allocate(64).expect("failed to register dst MR");
    let mut note = lb.pd.allocate(64).expect("failed to register note MR");
    let mut recv = lb.pd.allocate(64).expect("failed to register recv MR");
    payload.inner_mut()[..3].copy_from_slice(&[42, 43, 44]);
    note.inner_mut()[..2].copy_from_slice(&[1, 2]);

    unsafe { lb.qp.post_receive(&[recv.slice(..2)], 100) }.expect("post_receive failed");

    let payload_sge = [payload.slice(..3)];
    let note_sge = [note.slice(..2)];
    let remote = dst.remote().slice(..3);
    unsafe {
        lb.qp.post([
            WorkRequest::write(&payload_sge, remote, 101, None),
            WorkRequest::send(&note_sge, 102, None).signaled(),
        ])
    }
    .expect("batched post failed");

    // The receive completion for the send plus the single signaled send completion.
    let comps = drain(&lb.cq, 2);
    assert!(
        comps.iter().any(|wc| wc.wr_id() == 100),
        "missing recv completion"
    );
    assert!(
        comps.iter().any(|wc| wc.wr_id() == 102),
        "missing send completion"
    );
    assert_eq!(&dst.inner_mut()[..3], &[42, 43, 44]);
    assert_eq!(&recv.inner_mut()[..2], &[1, 2]);
}

/// Many outstanding work requests complete: post a batch of receives and sends, then confirm every
/// one produces a completion.
#[test]
fn multiple_outstanding() {
    require_device!("multiple_outstanding");
    let mut lb = loopback().expect("device requested but loopback not set up");

    const N: u64 = 8;
    // The memory regions must outlive their work requests, so keep them alive until after draining.
    let mut recv_mrs = Vec::new();
    for i in 0..N {
        let mr = lb.pd.allocate(8).expect("failed to register recv MR");
        unsafe { lb.qp.post_receive(&[mr.slice(..8)], 1000 + i) }.expect("post_receive failed");
        recv_mrs.push(mr);
    }
    let mut send_mrs = Vec::new();
    for i in 0..N {
        let mut mr = lb.pd.allocate(8).expect("failed to register send MR");
        mr.inner_mut()[0] = i as u8;
        unsafe { lb.qp.post_send(&[mr.slice(..8)], i) }.expect("post_send failed");
        send_mrs.push(mr);
    }

    let comps = drain(&lb.cq, (2 * N) as usize);
    for i in 0..N {
        assert!(
            comps.iter().any(|wc| wc.wr_id() == i),
            "missing send completion {i}"
        );
        assert!(
            comps.iter().any(|wc| wc.wr_id() == 1000 + i),
            "missing recv completion {i}"
        );
    }
}

/// The blocking completion-channel path (`wait`) returns completions just like polling does.
#[test]
fn wait_for_completion() {
    require_device!("wait_for_completion");
    let mut lb = loopback().expect("device requested but loopback not set up");

    let mut recv = lb.pd.allocate(16).expect("failed to register recv MR");
    let mut send = lb.pd.allocate(16).expect("failed to register send MR");
    send.inner_mut()[..4].copy_from_slice(b"wait");

    unsafe { lb.qp.post_receive(&[recv.slice(..4)], 1) }.expect("post_receive failed");
    unsafe { lb.qp.post_send(&[send.slice(..4)], 2) }.expect("post_send failed");

    // Collect both completions by blocking on the completion channel instead of busy-polling.
    let mut completions = [ibverbs::ibv_wc::default(); 8];
    let mut ids = Vec::new();
    while ids.len() < 2 {
        let done = lb
            .cq
            .wait(&mut completions, Some(Duration::from_secs(5)))
            .expect("wait failed");
        for wc in done.iter() {
            assert!(wc.error().is_none(), "work request {} failed", wc.wr_id());
            ids.push(wc.wr_id());
        }
    }
    assert!(
        ids.contains(&1) && ids.contains(&2),
        "missing completions: {ids:?}"
    );
    assert_eq!(&recv.inner_mut()[..4], b"wait");
}

/// An unreliable-connected (UC) queue pair carries SEND/RECV traffic to itself.
#[test]
fn unreliable_connection() {
    require_device!("unreliable_connection");
    let mut lb = loopback_of(ibv_qp_type::IBV_QPT_UC).expect("device requested but UC not set up");

    let mut recv = lb.pd.allocate(64).expect("failed to register recv MR");
    let mut send = lb.pd.allocate(64).expect("failed to register send MR");
    send.inner_mut()[..3].copy_from_slice(b"ucq");

    unsafe { lb.qp.post_receive(&[recv.slice(..3)], 1) }.expect("post_receive failed");
    unsafe { lb.qp.post_send(&[send.slice(..3)], 2) }.expect("post_send failed");

    let comps = drain(&lb.cq, 2);
    assert!(
        comps.iter().any(|wc| wc.wr_id() == 1),
        "missing recv completion"
    );
    assert!(
        comps.iter().any(|wc| wc.wr_id() == 2),
        "missing send completion"
    );
    assert_eq!(&recv.inner_mut()[..3], b"ucq");
}

/// Shared receive queue: the queue pair draws its receive buffers from an SRQ rather than its own
/// receive queue. This exercises a feature the crate exposes that some alternatives do not.
#[test]
fn shared_receive_queue() {
    require_device!("shared_receive_queue");
    let ctx = open_test_device().expect("device requested but not opened");

    let cq = ctx.create_cq(16, 0).expect("failed to create CQ");
    let pd = ctx.alloc_pd().expect("failed to allocate PD");
    let srq = pd.create_srq(16, 1, 0).expect("failed to create SRQ");

    let prepared = pd
        .create_qp(&cq, &cq, ibv_qp_type::IBV_QPT_RC)
        .expect("failed to create QP")
        .set_gid_index(1)
        .set_srq(&srq)
        .build()
        .expect("failed to build QP");
    let endpoint = prepared.endpoint().expect("failed to read endpoint");
    let mut qp = prepared.handshake(endpoint).expect("failed to connect QP");

    let mut recv = pd.allocate(64).expect("failed to register recv MR");
    let mut send = pd.allocate(64).expect("failed to register send MR");
    send.inner_mut()[..4].copy_from_slice(b"srq!");

    // Receives go to the SRQ, not the queue pair's own receive queue.
    unsafe { srq.post_receive(&[recv.slice(..4)], 1) }.expect("SRQ post_receive failed");
    unsafe { qp.post_send(&[send.slice(..4)], 2) }.expect("post_send failed");

    let comps = drain(&cq, 2);
    assert!(
        comps.iter().any(|wc| wc.wr_id() == 1),
        "missing SRQ recv completion"
    );
    assert!(
        comps.iter().any(|wc| wc.wr_id() == 2),
        "missing send completion"
    );
    assert_eq!(&recv.inner_mut()[..4], b"srq!");
    // Destroy the queue pair before the CQ/SRQ/PD it depends on, to avoid an EBUSY panic in `Drop`.
    drop(qp);
}
