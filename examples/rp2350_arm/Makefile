all:
	arm-none-eabi-gcc -mcpu=cortex-m33 -mcmse -mthumb -c -o startup.o startup.s
	cargo build --release
	mv target/thumbv8m.main-none-eabihf/release/rp2350_arm_demo rp2350_arm_demo.elf
	$(PICOTOOL_PATH) uf2 convert --verbose rp2350_arm_demo.elf demo.uf2 --family rp2350-arm-s

clean:
	cargo clean
	rm -f *.o

