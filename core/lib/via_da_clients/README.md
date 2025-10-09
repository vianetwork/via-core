# Via DA Clients

This crate contains the custom implementations of the Data Availability clients for Via Network.

Currently, the following DataAvailability clients are implemented:

- `Celestia client` that sends the pubdata to the Celestia network.

> blob_id : // [8]byte block height ++ [32]byte commitment

# Celestia Fallback to External Node - Quick Start Guide

## Overview

Celestia nodes only retain data for 30 days. This fallback mechanism allows the verifier network to query historical
data from the external node when Celestia data expires or is unavailable.

**Flow:**

```
Verifier → Try Celestia → If unavailable → Query External Node (Sequencer)
```

## Quick Setup

### 1. Configure Verifier

Add to `etc/env/l2-inits/via_verifier.init.env`:

```bash
# Fallback configuration
VIA_CELESTIA_CLIENT_FALLBACK_EXTERNAL_NODE_URL=http://sequencer-node:3050
VIA_CELESTIA_CLIENT_VERIFY_CONSISTENCY=true
```

**Variables:**

- `FALLBACK_EXTERNAL_NODE_URL`: RPC endpoint of your sequencer/external node
- `VERIFY_CONSISTENCY`: `true` = verify data matches (recommended for first 30 days), `false` = trust fallback data

### 2. Ensure RPC Client Config

In `etc/env/base/via_private.toml`:

```toml
[via_l2_client]
rpc_url = "http://sequencer-node:3050"
```

### 3. Compile and Restart

```bash
# Compile config
bin/via config compile via_verifier

# Restart verifier
bin/via verifier --network via_verifier
```

## Configuration Options

### Development/Testing

```bash
VIA_CELESTIA_CLIENT_FALLBACK_EXTERNAL_NODE_URL=http://localhost:3050
VIA_CELESTIA_CLIENT_VERIFY_CONSISTENCY=true
```

### Production

```bash
VIA_CELESTIA_CLIENT_FALLBACK_EXTERNAL_NODE_URL=https://sequencer.example.com:3050
VIA_CELESTIA_CLIENT_VERIFY_CONSISTENCY=true  # First 30 days
```

---
