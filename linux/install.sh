DEST_DIR=$HOME/bin/ipc-broker
EXECUTABLE_NAME=ipc-broker
SERVICE_NAME=$EXECUTABLE_NAME.service

set -e

echo "Uninstalling previous packages..."
# Check if systemd service exists before trying to stop/disable
if systemctl list-unit-files "$SERVICE_NAME" &>/dev/null; then
    echo "Stopping and disabling service: $SERVICE_NAME"
    sudo systemctl stop "$SERVICE_NAME"
    sudo systemctl disable "$SERVICE_NAME"
else
    echo "Service $SERVICE_NAME not found, skipping stop/disable"
fi

# Remove executable if it exists
if [ -f "$DEST_DIR/$EXECUTABLE_NAME" ]; then
    echo "Removing executable $DEST_DIR/$EXECUTABLE_NAME"
    sudo rm -f "$DEST_DIR/$EXECUTABLE_NAME"
fi

# Remove service file if it exists
if [ -f "/etc/systemd/system/$SERVICE_NAME" ]; then
    echo "Removing service file /etc/systemd/system/$SERVICE_NAME"
    sudo rm -f "/etc/systemd/system/$SERVICE_NAME"
fi

# Remove destination dir if it exists
if [ -d "$DEST_DIR" ]; then
    echo "Removing directory $DEST_DIR"
    sudo rm -rf "$DEST_DIR"
fi

echo "Build, Check, Test..."
cargo fmt --check || { echo "cargo fmt failed"; exit 1; }
cargo clippy || { echo "cargo clippy failed"; exit 1; }
cargo clippy --tests || { echo "cargo clippy --tests failed"; exit 1; }
cargo test --all-features || { echo "cargo test failed"; exit 1; }
cargo build --release || { echo "cargo build failed"; exit 1; }

echo "Installing new packages..."
sudo mkdir -p "$DEST_DIR"
sudo cp "$PWD/target/release/$EXECUTABLE_NAME" "$DEST_DIR"
sudo cp "$PWD/target/release/rob" "$DEST_DIR"
sudo cp "$PWD/linux/systemd/$SERVICE_NAME" "/etc/systemd/system/$SERVICE_NAME"
sudo chmod +x "$DEST_DIR/$EXECUTABLE_NAME"
sudo chmod +x "$DEST_DIR/rob"

sudo systemctl daemon-reload
sudo systemctl enable "$SERVICE_NAME"
sudo systemctl restart "$SERVICE_NAME"
sudo systemctl status "$SERVICE_NAME"