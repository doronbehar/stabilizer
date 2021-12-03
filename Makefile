build:
	cargo build --release
	cargo objcopy --release --bin lockin -- -O binary target/lockin.bin

target/lockin.bin: build

flash: target/lockin.bin
	sudo dfu-util -a 0 -s 0x08000000:leave -D target/lockin.bin

show-modulation:
	python -m miniconf --broker $$BROKER $$lockin 'output_conf/0="Modulation"'

.PHONY: flash show-modulation
