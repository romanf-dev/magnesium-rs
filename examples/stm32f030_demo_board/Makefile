all:
	arm-none-eabi-gcc -mcpu=cortex-m0 -mthumb -c -o startup.o startup.s
	cargo build --release

clean:
	cargo clean
	rm -f *.o

