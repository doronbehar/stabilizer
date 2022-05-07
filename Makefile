build:
	cargo build --release
	cargo objcopy --release --bin lockin -- -O binary target/lockin.bin

target/lockin.bin: build

flash: target/lockin.bin
	sudo dfu-util -a 0 -s 0x08000000:leave -D target/lockin.bin

test-final:
	@echo setting output 0 on \"Modulation\"
	@python -m miniconf --broker $$BROKER $$lockin 'output_conf/0="Modulation"'
	@#python -m miniconf --broker $$BROKER $$lockin 'afe/0="G2"'
	@echo setting output 0 to \"InPhase\"
	@python -m miniconf --broker $$BROKER $$lockin 'output_conf/1="InPhase"'
	@echo setting gain for analog front end gain of channel 1 to \"G10\"
	@python -m miniconf --broker $$BROKER $$lockin 'afe/1="G10"'
	@echo setting locking time constant to \"9\"
	@python -m miniconf --broker $$BROKER $$lockin 'lockin_tc=9'

.PHONY: flash test-final
