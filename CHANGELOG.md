# Changelog

Notable changes to the `ibverbs` and `ibverbs-sys` crates. Both follow [semantic
versioning](https://semver.org); while they are at `0.x`, a bump of the minor version may break
the API, and the patch version never does.

## ibverbs 0.10.0 (unreleased)

Every part of the public API changed since 0.9.2: the crate now drives the device through the
extended verbs, types queue pairs by transport, reference-counts resources instead of threading
lifetimes, and reports errors through its own `Error` enum. [Migrating from 0.9](#migrating-from-09)
below maps the old surface to the new one item by item.

### Added

- Queue pairs typed by transport (`Rc`, `Uc`, `Ud`, and `Srd` behind the `efa` feature): builder
  knobs, bring-up, and postable operations exist only on the transports they apply to, pinned by
  compile-fail tests. Datagram sends are addressed through an `AddressHandle`.
- Doorbell batches (`QueuePair::start_send`, `SendBatch`, `SendOp`): many work requests, one
  doorbell, with `signaled`/`fenced`/`solicited` modifiers, inline data (single buffer or
  gathered), RDMA read and write (with immediate), and atomic compare-and-swap / fetch-and-add.
- Batched receives (`RecvRequest`, `QueuePair::post_recv`) linking the caller's own array, and
  shared receive queues (`ProtectionDomain::create_srq`, `SharedReceiveQueue::post_recv`).
- Completion queues on the extended interface: a builder (`Context::create_cq(n).build()`) with
  completion-vector, context-cookie, and work-completion-field settings; lazy-reading lent
  completions (`CompletionQueue::poll`); `poll_into` for the classic `ibv_wc` batch; hardware
  completion timestamps and the extended completion fields; completion channels
  (`Context::create_comp_channel`) that many queues share and that expose their descriptor for
  `epoll`/`tokio`; device asynchronous events (`Context::wait_async_event`, `poll_async_event`,
  `async_fd`).
- The RDMA connection manager (`rdmacm` feature): blocking `Connector`/`Acceptor`/`Connection`
  helpers with timeouts, private data in both directions, and `reject`, plus the low-level
  `CmId`/`CmEvent` API for event loops (non-blocking, `AsFd`).
- AWS Elastic Fabric Adapter SRD queue pairs (`efa` feature).
- Device introspection: `Context::query_device`, `query_device_ex`, `query_port` with typed
  attribute wrappers (`DeviceAttr`, `PortAttr`, `PortSpeed`, `Mtu`, ...), `gid_table`,
  `query_gid`, GID-to-netdev resolution, `Device::transport_type`, `Debug`/`Display` throughout,
  and a `devinfo` example.
- Memory: `ProtectionDomain::register_from_raw` for caller-managed memory, `register_dmabuf` for
  device memory (registered at its IOVA), `advise_mr`, `MemoryRegion::lkey`, and
  `RemoteMemorySlice` with sub-slicing and a wire encoding for one-sided operations.
- General queue-pair control: `QueuePair::modify`/`query` with `QueuePairAttribute`, validated
  against the kernel's state table so a rejected transition names the wrong attribute bits;
  `PreparedQueuePair::into_queue_pair` for manual bring-up; queue pairs on a chosen device port;
  typed `AckTimeout`/`RnrTimer`; `QueuePairBuilder::set_sq_psn`.
- Raw escape hatches: `as_raw` on every wrapper and the `ibverbs-sys` bindings re-exported as
  `ibverbs::ffi`.
- A stable wire format for `QueuePairEndpoint` and `RemoteMemorySlice` (`to_bytes`/`from_bytes`)
  that fits an rdmacm request's private data.
- A typed `Error` enum (`thiserror`) naming the failing verb, with `Error::Unsupported` for
  operations the device or provider lacks.
- A Bazel build; CI that runs the data-path tests against a Soft-RoCE device, builds with the
  vendored headers the way a crates.io consumer does, and checks packaging.

### Changed

- Resources are reference counted: `Context`, `ProtectionDomain`, `CompletionQueue`, and
  `QueuePair` carry no lifetime parameters and keep what they depend on alive.
- Queue pairs and completion queues are created through `ibv_create_qp_ex`/`ibv_create_cq_ex`
  and sends are posted through the `ibv_wr_*` API, so the provider must implement the extended
  verbs (`mlx5`, `hns`, `efa`, `rxe` do); otherwise creation fails with `Error::Unsupported`.
- `MemoryRegion` is bytes-only and generic over its buffer owner; `allocate` takes a size in
  bytes and access flags.
- `serde` support was replaced by the `to_bytes`/`from_bytes` wire format.
- The raw `ffi` enums and flags are wrapped in crate-native types (`QueuePairType`,
  `QueuePairState`, `AccessFlags`, `WcStatus`, `WcOpcode`, `GidType`, `Mtu`, ...); the
  root-level `ffi` re-exports are gone.
- `GidEntry::port_num` is a `u8`; `QueuePairEndpoint` gained a `psn` field.
- The `rdma-core` build no longer compiles anything: `ibverbs-sys` only runs cmake's configure
  step to generate the vendored headers.

### Fixed

- `Device::guid`, `DeviceAttr::node_guid`, and `sys_image_guid` reversed the GUID's bytes on
  little-endian hosts.
- A datagram send's address handle could be dropped between `SendBatch::to` and the opcode call;
  it is now borrowed until the request is posted.
- `ffi::ibv_qp_attr::default()` produced an invalid `path_mtu` value; the sys crate now provides
  a valid default, used everywhere attributes are built.
- `Gid` and `Guid` no longer hand out misaligned references to the raw union types.
- The receive-queue PSN could not be paired with the peer's send-queue PSN: the endpoint now
  carries the PSN and `handshake` applies it.
- Queue-pair creation reports `Error::Unsupported` for `EOPNOTSUPP`, as documented.
- `efa` builds against rdma-core 64 (the EFA and extended-CQ attribute structs are no longer
  written as exhaustive literals).
- The default build pointed bindgen at a directory that did not exist, so the host's rdma-core
  headers were silently used instead of the vendored ones.
- rdmacm: the active side re-applies the `INIT` attributes once the connection exists, so the
  peer's RDMA writes and reads into it are allowed; `Incoming::accept` configures the queue pair
  with the outstanding-RDMA limits it advertises; `Connection::disconnect` flushes the queue pair
  and dropping a `Connection` disconnects immediately; a `CmEvent` keeps its `CmId` alive (so
  destroying the id cannot block forever) and an untaken connection request is rejected on drop
  instead of leaking; private data is bounded per call and port space with a proper error;
  failure events carry their status and private data in `Error::ConnectionManager`; the blocking
  helpers refuse the datagram port spaces they cannot serve.

## ibverbs-sys 0.4.0+63.0 (unreleased)

### Changed

- The vendored `rdma-core` moved from v55.0 to v63.0, adding enum variants and struct fields.
- Enums are generated as Rust enums (they were modules of constants), so `ibv_wc::error` and
  `ibv_wc::opcode` return `ibv_wc_status`/`ibv_wc_opcode` values directly.
- `ibv_qp_attr` has a hand-written `Default` with a valid `path_mtu`.
- The build script runs cmake's configure step only (nothing is compiled; the system
  `libibverbs` is linked), honours `RDMA_CORE_INCLUDE_DIR`/`RDMA_CORE_LIB_DIR` for pre-built
  headers and `IBVERBS_HEADER_DIR` for `verbs.h`, and exports the generated include directory to
  dependents as `DEP_IBVERBS_INCLUDE`. The `cmake` crate is no longer a build dependency; the
  `cmake` binary is still required.

### Added

- Wrappers for the `static inline` verbs that dispatch through the provider op table:
  `ibv_create_cq_ex`, `ibv_create_qp_ex`, `ibv_advise_mr`, `ibv_query_rt_values_ex`,
  `ibv_query_device_ex`, and `___ibv_query_port` (the exported `ibv_query_port` symbol fills only
  the legacy attributes). They report a missing provider op as `EOPNOTSUPP` like the C inlines.
- The `rdmacm` feature (bindings for `rdma_cma.h`, plus `rdma_get_local_addr`/`rdma_get_peer_addr`)
  and the `efa` feature (bindings for `efadv.h`), each linking the corresponding library.
- Bitfield newtypes for the remaining flag enums (`ibv_create_cq_wc_flags`,
  `ibv_qp_create_send_ops_flags`, `ibv_wc_flags`, ...).

## Migrating from 0.9

| 0.9.2 | 0.10.0 |
| --- | --- |
| `io::Result<T>` everywhere | `ibverbs::Result<T>` with the `ibverbs::Error` enum; `raw_os_error() == Some(EOPNOTSUPP)` is `Error::Unsupported` |
| `CompletionQueue<'ctx>`, `ProtectionDomain<'ctx>`, `QueuePair<'res>`, ... | No lifetime parameters; handles are reference counted and can be moved into your own types |
| `ctx.create_cq(n, id)` | `ctx.create_cq(n).set_context(id).build()?` |
| `cq.poll(&mut wcs)?` over `ibv_wc` | `if let Some(mut c) = cq.poll()? { while let Some(wc) = c.next() { .. } }`; `wc.ok()` replaces `is_valid()`/`error()`, `wc.opcode()` is a `WcOpcode`. `cq.poll_into(&mut wcs)` keeps the batch shape |
| `pd.create_qp(&send, &recv, ibv_qp_type::IBV_QPT_RC)` | `pd.create_qp::<Rc>(&send, &recv, port_num)?` (`Uc`, `Ud`, `Srd` likewise); the port is explicit |
| `.set_access(ibv_access_flags::..)`, `.allow_remote_rw()` | `.set_access(AccessFlags::LOCAL_WRITE \| AccessFlags::REMOTE_WRITE \| ..)` or `AccessFlags::PERMISSIVE` |
| `.set_path_mtu(u32)`, `.set_timeout(u8)`, `.set_min_rnr_timer(u8)` | `.set_path_mtu(Mtu)`, `.set_timeout(AckTimeout)`, `.set_min_rnr_timer(RnrTimer)` |
| `.set_rq_psn(psn)` | `.set_sq_psn(psn)`: the endpoint carries it and `handshake` sets the peer's receive-queue PSN from it |
| `prepared.endpoint()` and `serde` on `QueuePairEndpoint` | `prepared.endpoint()?`; exchange `endpoint.to_bytes()` and decode with `QueuePairEndpoint::from_bytes` |
| `pd.allocate::<T>(n)?` → `MemoryRegion<T>`, indexed as `[T]` | `pd.allocate(bytes, AccessFlags::..)?` → `MemoryRegion<Box<[u8]>>`; `mr.bytes()`/`mr.bytes_mut()`; reinterpret with your own casting (for example `bytemuck`) |
| `MemoryRegion::register(..)` for existing memory | `unsafe { pd.register_from_raw(ptr, len, flags) }` |
| `mr.rkey()` → `RemoteKey` | `mr.rkey()` → `u32`; `mr.remote()` → `RemoteMemorySlice` for RDMA read/write targets |
| `unsafe { qp.post_send(&mut mr, range, wr_id) }` | `let mut batch = qp.start_send(); batch.op().signaled().send(wr_id, &[mr.slice(range)]); unsafe { batch.submit() }?` (requests are unsignaled unless marked) |
| `unsafe { qp.post_receive(&mut mr, range, wr_id) }` | `unsafe { qp.post_recv([RecvRequest::new(wr_id, &[mr.slice(range)])]) }?` |
| `ctx.gid_table()` → `&[GidEntry]` | `ctx.gid_table()?` → `Vec<GidEntry>` (queried on demand); `GidEntry::port_num` is a `u8` |
| `ibverbs::ibv_qp_type`, `ibv_wc_status`, `ibv_gid_type`, `ibv_access_flags` re-exports | `QueuePairType`, `WcStatus`, `GidType`, `AccessFlags`; the raw types stay reachable as `ibverbs::ffi::..` |
| `Device::guid()` | Same call; the bytes are now in the order `ibv_devinfo` shows |
| Drop of a `CompletionQueue` still referenced by a `QueuePair` panicked | Cannot happen: the queue pair keeps its completion queues alive |

Provider note: 0.10 needs a provider with the extended verbs. On `mlx4`-generation and other
legacy-only providers, `create_cq`/`create_qp` fail with `Error::Unsupported` where 0.9 worked.
