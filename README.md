Simple hardware-assisted asynchronous micro-RTOS in Rust
========================================================

Magnesium is a simple kernel implementing CSP-like computation 
model for deeply embedded systems. It uses interrupt controller hardware for 
scheduling and supports preemptive multitasking. This is an experimental version 
of the framework in Rust.

Unlike the original [magnesium framework](https://github.com/romanf-dev/magnesium) 
written in C, Rust version uses async functions and futures to make code more readable.
Also, it uses no external crates except core::, no need for nightly, no macros, 
etc. so it may be helpful for those who want to dive into details.


Features
--------

- Preemptive multitasking
- Hardware-assisted scheduling
- Unlimited number of actors and queues
- Zero-copy message-passing communication
- Timer facility
- Hard real-time capability
- Basic support for symmetric multiprocessing
- Only ARMv6-M and ARMv7-M are supported at now


API usage examples
------------------

It is all about messages. Messages are declated as regular structs embedded into 
the Message type:

        struct ExampleMsg {
            n: u32
        }

        const MSG_PROTOTYPE: Message<ExampleMsg> = Message::new(ExampleMsg { n: 0 });
        static mut MSG_STORAGE: [Message<ExampleMsg>; 5] = [MSG_PROTOTYPE; 5];


Message pools are used to allocate a message. Unsafe is required only once 
for pool initialization as it is accessing static mut.
Messages may be allocated from a pool either synchronously using an 'alloc' 
method or asynchronously using a 'get'.

        static POOL: Pool<ExampleMsg> = Pool::new();
        unsafe { POOL.init(addr_of_mut!(MSG_STORAGE)); }

        
Channels are used for data exchange. Channel read is an asynchronous operation, 
while write is always synchronous. Channels have no internal storage, they are just 
heads of intrusive lists of messages.

        static CHAN: Channel<ExampleMsg> = Channel::new();
        CHAN.init();


Messages are pushed into queues using the 'put' method to notify 
corresponding actors waiting for these channels.

        let mut msg = POOL.alloc().unwrap();
        /* ... set message payload ... */
        CHAN.put(msg);


Channels are awaited using shared references:

        async fn foo() {
            let msg = CHAN.block_on().await;
            ...
        }

Actors (or message handlers) are mapped to hardware interrupts. 
Note that the interrupt controller should be carefully adjusted to 
reflect IRQ->priority relations.
Actor's function is an infinite loop just like a thread:

        async fn blinky() -> ! {
            let q = &CHAN;
            loop {
                let msg = q.block_on().await;
                ...
            }
        }

There are no restrictions on actor functions: they may accept any parameters, may be 
generic, etc.

Finally, there is an executor (or scheduler) representing a set of 
ready-to-run actors. Executor has two generic parameters: number of
priorities and number of CPUs available. Below is an example of 
single-CPU executor supporting 10 priorities:

        static SCHED: Executor<10, 1> = Executor::new();


It has to be initialized once at startup and then called inside 
interrupt handlers designated to actors execution:

        SCHED.schedule(...<current interrupt vector number>...);


The main function must contain scheduler launch as its last statement:

        let mut future = blinky();
        ...

        SCHED.run({[
            (<vector number 0>, &mut future)
            ...
            (<vector number N>, &mut <future N>)
            ...
        ]});


Note that this function expects disabled interrupts (and panics in case 
when they are enabled) since this may lead to undefined behavior.

The framework also provides timer facility. All software timers are 
handled by single global timer object representing a tick source.

        static TIMER: Timer< number of hierarchy levels, number of CPUs > = Timer::new();


Timers form a hierarchy based on their timeout value. Default value is 10. 
After the global context is initialized in the main using the 'init' method:

        TIMER.init();


then some interrupt must provide periodic ticks using tick function:

        TIMER.tick();


After that any actor may use sleep_for method with some amount of ticks 
to sleep:

        loop {
            TIMER.sleep_for(10).await; // Sleep for 10 ticks.
        }


Tick duration is unspecified and depends on your settings.


Safety notes
------------

All static-mut-objects are only accessed once during initialization, 
normal operational code of either actors or interrupts is safe and does not 
require unsafe code. Also the user is responsible for NVIC initialization and 
proper content of the vector table. 
Executor transmutes future objects and the array of active futures into 
static lifetime despite they're allocated on the stack, however, since 'run' 
function does not return and all the actors are infinite loops this is 
considered safe.


Demo
----

The demo is a toy example with just one actor which blinks the LED. 
Use 'make' to build.

