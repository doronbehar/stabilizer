# Getting Started

There are a number of steps that must be completed when first getting started with Stabilizer.
1. Update the Stabilizer Application
    1. Build the application by compiling the source code.
    1. Upload the application and programming it onto the device.
1. Set up MQTT for telemetry and configuration.

The following sections will walk you through completing each of these steps.

# Update Stabilizer

Firmware is compiled and loaded onto Stabilizer to program a specific application.

After receiving the Stabilizer hardware, you will need to flash firmware onto the device to use your
desired application.

## Building Firmware

1. Clone or download [stabilizer](https://github.com/quartiq/stabilizer)
```bash
git clone https://github.com/quartiq/stabilizer
```
2. Get [rustup](https://rustup.rs/)
    * The minimum supported Rust version (MSRV) is 1.52.0

3. Install target support
```bash
rustup target add thumbv7em-none-eabihf
```
4. Build Firmware with an optionally-specified MQTT broker IP.
```bash
# Bash
BROKER="10.34.16.10" cargo build --release

# Powershell
# Note: This sets the broker for all future builds as well.
$env:BROKER='10.34.16.10'; cargo build --release
```

## Uploading Firmware

Firmware is loaded onto stabilizer utilizing an ST-Link (V2-1 or greater) JTAG programmer.
* If a programmer is not available, please see the [alternative method](#alternative-using-usb)

Ensure the ST-Link is connected to Stabilizer as shown below.

![JTAG Connection](assets/stabilizer-jtag.jpg)

All of the instructions below assume you have properly [`built the firmware`](#building-firmware).

Substitute `dual-iir` below with the application name you are flashing.

1. Install [cargo-binutils](https://github.com/rust-embedded/cargo-binutils/)
```bash
cargo install cargo-binutils
rustup component add llvm-tools-preview
```

2. Generate the binary file
```bash
# Bash
BROKER="10.34.16.10" cargo objcopy --release --bin dual-iir -- -O binary dual-iir.bin

# Powershell
$env:BROKER='10.34.16.10'; cargo objcopy --release --bin dual-iir -- -O binary dual-iir.bin
```

3. Copy `dual-iir.bin` into the ST-Link drive on your computer.


### Alternative: Using USB

If an ST-Link V2-1 or above is not available, you can upload firmware using a micro USB cable
plugged in to the front of Stabilizer.

1. Install the DFU USB tool [`dfu-util`](http://dfu-util.sourceforge.net)
1. Connect to the Micro USB connector below the RJ45
1. Short JC2/BOOT
1. Perform the Device Firmware Upgrade (DFU)
```bash
dfu-util -a 0 -s 0x08000000:leave -D dual-iir.bin
```

### Alternative: Firmware Development / Debug

The below instructions are useful for debugging or developing firmware

For an interactive flash experience with live logging, utilize `probe-run` as follows.

1. Install `probe-run`
```bash
cargo install probe-run
```
2. Build and run firmware on the device
```bash
# Bash
BROKER="10.34.16.10" cargo run --release --bin dual-iir

# Powershell
$Env:BROKER='10.34.16.10'; cargo run --release --bin dual-iir
```
3. When using debug (non `--release`) mode, decrease the sampling frequency significantly.
  The added error checking code and missing optimizations may lead to the code
  missing deadlines and panicing.

# Set up and test MQTT

## Set up the Broker
Stabilizer requires an MQTT broker that supports MQTTv5. [Mosquitto](https://mosquitto.org/) has
been used during development, but any MQTTv5 broker is supported.

> _Note_: Mosquitto version 1 only supports MQTTv3.1. If using Mosquitto, ensure version 2.0.0 or
> later is used.

Stabilizer utilizes a static IP address for broker configuration. Ensure the IP address was
[configured](#building-firmware) properly to point to your broker before continuing.

We recommend running Mosquitto through [Docker](https://docker.com) to easily run Mosquitto on
Windows, Linux, and OSX. After docker has been installed, run the following command from
the `stabilizer` repository:
```bash
# Bash
docker run -p 1883:1883 --name mosquitto -v `pwd`/mosquitto.conf:/mosquitto/config/mosquitto.conf -v /mosquitto/data -v /mosquitto/log eclipse-mosquitto:2

# Powershell
docker run -p 1883:1883 --name mosquitto -v ${pwd}/mosquitto.conf:/mosquitto/config/mosquitto.conf -v /mosquitto/data -v /mosquitto/log eclipse-mosquitto:2
```

This command will create a container named `mosquitto` that can be stopped and started easily via
docker.

## Test the Connection
Once your broker is running, test that Stabilizer is properly connected to it.

To do this, we will check that Stabilizer is reporting telemetry on the following topic:
```
dt/sinara/dual-iir/00-11-22-33-44-55/telemetry
```
> **Note**: The telemetry topic will be different based on the programmed application and the MAC address
   of the device.

Download [MQTT-Explorer](http://mqtt-explorer.com/) to observe which topics have been posted to the
Broker.

![MQTT Explorer Configuration](assets/mqtt-explorer.png)

> **Note**: Use the same broker address that you defined in the firmware for MQTT explorer.

Telemetry messages should come in approximately every 10 seconds when Stabilizer has connected to
the broker.  Once you observe incoming telemetry, Stabilizer has been properly configured and is
operational.
