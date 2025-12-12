
%.o : %.c
	arm-none-eabi-gcc -mcpu=cortex-m33 -mcmse -mthumb -c -o $@ $<

rp2350_arm_demo.uf2 : rp2350_arm_demo.elf
	$(PICOTOOL_PATH) uf2 convert --verbose $< $@ --family rp2350-arm-s

rp2350_arm_demo.elf : startup.o
	cargo build --release
	mv target/thumbv8m.main-none-eabihf/release/rp2350_arm_demo $@

all : rp2350_arm_demo.uf2
	echo OK

clean:
	cargo clean
	rm -f *.uf2 *.elf 

