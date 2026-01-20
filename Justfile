# Default recipe to run when just is called without arguments
default:
    just --list

# Build protocol buffers
proto:
    cargo build -p sova-sentinel-proto

# Clean protocol buffer generated files
clean-proto:
    cargo clean -p sova-sentinel-proto

# Build entire workspace
build:
    cargo build

# Build and run tests
test:
    cargo test

# Run tests with verbose output
test-verbose:
    cargo test -- --nocapture --test-threads=1

# Run tests for a specific crate
test-crate crate:
    cargo test -p {{crate}} -- --nocapture

# Run the server
server:
    RUST_LOG=debug cargo run -p sova-sentinel-server

# Run the example client
client:
    cargo run -p sova-sentinel-client --example client

# Run server with custom configuration
server-custom port db_path:
    RUST_LOG=debug SOVA_SENTINEL_PORT={{port}} SOVA_SENTINEL_DB_PATH={{db_path}} cargo run -p sova-sentinel-server

# Start server with a fresh database (backs up existing db)
clean:
    #!/usr/bin/env sh
    # Search for .db or .sqlite files in root and data directories
    DB_FILES=$(find . -maxdepth 1 -type f \( -name "*.db" -o -name "*.sqlite" \) 2>/dev/null)
    DATA_FILES=$(find ./data -maxdepth 1 -type f \( -name "*.db" -o -name "*.sqlite" \) 2>/dev/null)
    
    if [ -n "$DB_FILES" ]; then
        DB_PATH=$(echo "$DB_FILES" | head -n1)
        DB_PATH="${DB_PATH#./}"  # Remove leading ./
    elif [ -n "$DATA_FILES" ]; then
        DB_PATH=$(echo "$DATA_FILES" | head -n1)
        DB_PATH="${DB_PATH#./}"  # Remove leading ./
    fi
    
    # Only process existing database if we found one
    if [ -n "$DB_PATH" ]; then
        # Extract path and filename without extension
        DB_DIR=$(dirname "$DB_PATH")
        DB_FILE=$(basename "$DB_PATH")
        DB_NAME=$(echo "$DB_FILE" | sed 's/\.[^.]*$//')
        
        # Get file extension or default to .db
        DB_EXT=$(echo "$DB_FILE" | grep -o '\.[^.]*$' || echo '.db')
        
        # Create backup with timestamp if file exists
        if [ -f "$DB_PATH" ]; then \
            BACKUP_PATH="$DB_DIR/$DB_NAME.backup-$(date +%Y%m%d-%H%M%S)$DB_EXT"
            mv "$DB_PATH" "$BACKUP_PATH"; \
            echo "Backed up database to: $BACKUP_PATH"; \
        fi
    fi
