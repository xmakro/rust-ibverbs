//! Minimal EFA SRD example: two SRD queue pairs on one device, where one RDMA-writes into a memory
//! region owned by the other.
//!
//! This requires an AWS Elastic Fabric Adapter (EFA) device and the `efa` feature
//! (`cargo run --features efa --example efa_srd`). It is illustrative: it exercises the public SRD
//! API end to end, but the exact device behaviour should be validated on real EFA hardware.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const GID_INDEX: u32 = 0;
    const QKEY: u32 = 0x1111_2222;

    let device = ibverbs::devices()?
        .iter()
        .next()
        .ok_or("no RDMA device found")?
        .open()?;
    let cq = device.create_cq(16, 0)?;
    let pd = device.alloc_pd()?;

    // Two SRD queue pairs on the same device. SRD is connectionless, so each is just brought to
    // ready with a Q_Key; no handshake is exchanged.
    let writer_prepared = pd
        .create_srd_qp(&cq, &cq)?
        .set_gid_index(GID_INDEX)
        .build_srd()?;
    let writer_endpoint = writer_prepared.endpoint()?;
    let mut writer = writer_prepared.activate_srd(QKEY)?;

    let target_prepared = pd
        .create_srd_qp(&cq, &cq)?
        .set_gid_index(GID_INDEX)
        .build_srd()?;
    let target_endpoint = target_prepared.endpoint()?;
    let _target = target_prepared.activate_srd(QKEY)?;

    // The writer addresses the target with an address handle pointing at the target's GID.
    let target_gid = target_endpoint.gid.ok_or("EFA requires a GID")?;
    let mut ah_attr = ibverbs::AddressHandleAttribute::new();
    ah_attr.set_grh(target_gid, GID_INDEX as u8, 64, 0);
    let ah = pd.create_address_handle(&ah_attr)?;

    // Source data in the writer's region, and a destination region owned by the target side.
    let mut source = pd.allocate(64)?;
    source.inner_mut()[..5].copy_from_slice(b"hello");
    let mut destination = pd.allocate(64)?;
    let remote = destination.remote().slice(..5);

    // SAFETY: `source` and `destination` outlive the work request and are not moved or freed before
    // its completion is polled below.
    unsafe {
        writer.post_write_srd(
            &[source.slice(..5)],
            remote,
            &ah,
            target_endpoint.num,
            QKEY,
            1,
        )?;
    }

    // Wait for the (signaled) write to complete.
    let mut completions = [ibverbs::ibv_wc::default(); 4];
    loop {
        let done = cq.poll(&mut completions)?;
        if let Some(wc) = done.iter().find(|wc| wc.wr_id() == 1) {
            if let Some((status, vendor_err)) = wc.error() {
                return Err(format!("write failed: {status:?} (vendor {vendor_err})").into());
            }
            break;
        }
    }

    println!(
        "wrote {:?} into the target region",
        std::str::from_utf8(&destination.inner_mut()[..5])?
    );
    let _ = writer_endpoint;
    Ok(())
}
