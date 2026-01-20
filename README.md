# Sova Sentinel

A gRPC service for managing storage slot locks in EVM smart contracts on Sova.

## Project Structure

```
sova-sentinel/
├── Cargo.toml
└── crates/
    ├── proto/          # Protocol definitions and generated gRPC code
    ├── client/         # Client library for interacting with the service
    └── server/         # Server implementation with SQLite backend
```

## Crates Overview

- **sova-sentinel-proto**: Contains the protobuf service definitions and generated gRPC code.
- **sova-sentinel-client**: Provides a Rust client library for interacting with the service.
- **sova-sentinel-server**: Implements the gRPC service with a SQLite backend.

## Getting Started

### Clone the Repository

```bash
git clone https://github.com/sovafoundation/sova-sentinel.git
cd sova-sentinel
```

### Configuration

The server can be configured using environment variables or a `.env` file.

Create a `.env` file in the root directory:
```
SOVA_SENTINEL_HOST=[::1]
SOVA_SENTINEL_PORT=50051
SOVA_SENTINEL_DB_PATH=data/db.sqlite
BITCOIN_RPC_URL=http://localhost:18443
BITCOIN_RPC_USER=user
BITCOIN_RPC_PASS=password
BITCOIN_RPC_CONNECTION_TYPE=bitcoincore
BITCOIN_CONFIRMATION_THRESHOLD=6
BITCOIN_REVERT_THRESHOLD=18
BITCOIN_RPC_MAX_RETRIES=5
```

Available configuration options:
- `SOVA_SENTINEL_HOST`: Host for the gRPC server (default: `[::1]`)
- `SOVA_SENTINEL_PORT`: Port for the gRPC server (default: 50051)
- `SOVA_SENTINEL_DB_PATH`: Path to the SQLite database file (default: slot_locks.db)
- `BITCOIN_RPC_URL`: Bitcoin node RPC URL (default: http://localhost:18443)
- `BITCOIN_RPC_USER`: Bitcoin node RPC username (default: user)
- `BITCOIN_RPC_PASS`: Bitcoin node RPC password (default: pass)
- `BITCOIN_RPC_CONNECTION_TYPE`: RPC connection type (`bitcoincore` or `external`, default: `bitcoincore`)
- `BITCOIN_CONFIRMATION_THRESHOLD`: Number of confirmations required to unlock a slot (default: 6)
- `BITCOIN_REVERT_THRESHOLD`: Number of blocks after which a locked slot will revert (default: 18)
- `BITCOIN_RPC_MAX_RETRIES`: Maximum number of retries for Bitcoin RPC calls (default: 5)

### Building and Running

The project uses [Just](https://github.com/casey/just) as a command runner. There are other options shown below for running the service that do not require just.

Available commands:
```bash
just proto           # Build protocol buffers
just clean-proto     # Clean generated protobuf code
just build           # Build entire workspace
just test            # Run all tests
just server          # Run the server with default settings
just client          # Run the example client
just server-custom 3000 ./my.db  # Run server with custom port and database path
```

You can see all available commands with:
```bash
just --list
```

### Running With Cargo

If you prefer not to use Just, you can run the commands directly:

Using default configuration:
```bash
cargo run -p sova-sentinel-server
```

Using custom configuration:
```bash
SOVA_SENTINEL_PORT=3000 SOVA_SENTINEL_DB_PATH=/path/to/db.sqlite cargo run -p sova-sentinel-server
```

Running the example client:
```bash
cargo run -p sova-sentinel-client --example client
```

### Running with Docker

Build the Docker image:
```bash
docker build -t sova-sentinel .
```

Run the container:
```bash
docker run -d \
  --name sova-sentinel \
  -p 50051:50051 \
  -v sova-data:/app/data \
  -e BITCOIN_RPC_URL=http://your-bitcoin-node:8332 \
  -e BITCOIN_RPC_USER=your-username \
  -e BITCOIN_RPC_PASS=your-password \
  --network your-bitcoin-network \
  sova-sentinel
```

## Client Library

To use the client library in your project:

1. Clone this repository:
   ```bash
   git clone git@github.com:sovafoundation/sova-sentinel.git
   cd sova-sentinel
   ```

2. Build the protocol buffers:
   ```bash
   just proto
   ```

3. Add the client library to your project's `Cargo.toml`:
   ```toml
   [dependencies]
   sova-sentinel-client = { path = "path/to/sova-sentinel/crates/client" }
   ```

4. See the [example client](crates/client/examples/client.rs) for usage details.

## Operations

### Single Slot Operations
- `lock_slot`: Lock a slot with revert value and current value
- `get_slot_status`: Check if a slot is locked, unlocked, or reverted

### Batch Operations
- `batch_lock_slot`: Lock multiple slots in a single transaction
- `batch_get_slot_status`: Get status of multiple slots efficiently
- `batch_unlock_slot`: (Development Only) Force unlock multiple slots without BTC confirmation

## Example Usage

### Single Slot Operations
```rust
// Check slot status
let status_response = client.get_slot_status(
    current_block,       // Current block
    contract_address,    // Contract address
    slot_index,          // Slot index as bytes
).await?;

// Lock a single slot
let lock_response = client.lock_slot(
    locked_at_block,     // Block where lock takes effect
    contract_address,    // Contract address
    slot_index,          // Slot index as bytes
    revert_value,        // Value to revert to if BTC tx fails
    current_value,       // Current value of the slot
    btc_txid,            // Bitcoin transaction ID
    btc_block,           // Bitcoin block number
).await?;
```

### Batch Operations
```rust
use sova_sentinel_proto::proto::{SlotData, SlotIdentifier};

// Lock multiple slots
let lock_response = client.batch_lock_slot(
    locked_at_block,    // Block where locks take effect
    btc_block,          // Bitcoin block number
    vec![               // Vec<SlotData>
        SlotData {
            contract_address: "0x123".to_string(),
            slot_index: slot_index,
            revert_value: revert_bytes,
            current_value: current_bytes,
            btc_txid: "txid1".to_string(),
        },
        // ... more slots ...
    ],
).await?;

// Check status of multiple slots
let status_response = client.batch_get_slot_status(
    current_block,
    btc_block,
    vec![               // Vec<SlotIdentifier>
        SlotIdentifier {
            contract_address: "0x123".to_string(),
            slot_index: slot_index,
        },
        // ... more slots ...
    ],
).await?;

// Development Only: Force unlock slots without BTC confirmation
let unlock_response = client.batch_unlock_slot(
    current_block,
    btc_block,
    vec![               // Vec<SlotIdentifier>
        SlotIdentifier {
            contract_address: "0x123".to_string(),
            slot_index: slot_index,
        },
        // ... more slots ...
    ],
).await?;
```

Note: The `batch_unlock_slot` operation is provided for development convenience only. In production, slots should be unlocked through the normal Bitcoin confirmation process using `batch_get_slot_status`.


## Retry Behavior

The service implements an exponential backoff retry strategy for Bitcoin RPC calls:
- Base delay starts at 100ms and doubles with each retry
- Jitter is added to prevent thundering herd problems
- Only connectivity errors are retried (other errors fail immediately)
- Maximum retries is configurable via `BITCOIN_RPC_MAX_RETRIES`
- After max retries, returns a gRPC `UNAVAILABLE` status code with a `BitcoinNodeUnreachable` error message

## Development

### Running Tests
```bash
cargo test
```
