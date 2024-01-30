Simple hardware-assisted asynchronous micro-RTOS in Rust
========================================================

DRAFT

Overview
--------

The framework relies on hardware features of the NVIC interrupt controller: the ability to set user-defined priorities to IRQs and possibility to trigger interrupts programmatically. Actor's priorities are mapped to interrupt vectors and interrupt controller acts as a hardware scheduler. Therefore, interrupts are used as 'execution engine' for actors with certain priority. The idea is to re-use unused IRQ vectors as application actors.

When some event (hardware interrupt) occurs, it allocates and posts the message to a queue, this causes activation of the subscribed actor, moving the message into its incoming mailbox, moving the actor to the list of ready ones and also triggering interrupt vector corresponding to actor's priority. The hardware automatically transfers control to the activated vector, its handler then calls framework's function 'schedule' which eventually calls activated actors.

Unlike the original version of the framework written in C, Rust version uses async functions and futures to make code more safe and readable.
Also, it uses no external crates except core::, no need for nightly, no macros, etc. so it may be helpful for those who want to dive into details.


API description
---------------

Actor model is all about messages. Messages are declated as usual structs embedded into the Message type:

        struct ExampleMsg {
            n: u32
        }

        const MSG_PROTOTYPE: Message<ExampleMsg> = Message::new(ExampleMsg { n: 0 });
        static mut MSG_STORAGE: [Message<ExampleMsg>; 5] = [MSG_PROTOTYPE; 5];


Message pools are used to allocate a message. Unsafe is required only for access to 'static mut' once during initialization.

        static POOL: Pool<ExampleMsg> = Pool::new();
        ...
        POOL.init(unsafe {&mut MSG_STORAGE});

        
Pools may be used by actors and interrupt handlers but they're not awaitable objects. The right object for the data exchange is the queue. Queues are just heads of
internal intrusive lists, they have no internal storage.

        static QUEUE: Queue<ExampleMsg> = Queue::new();
        ...
        QUEUE.init();


Messages should be posted to the queues using the put method to notify corresponding actors awaiting those queues.

        let mut msg = POOL.alloc().unwrap();
        ... set message payload ...
        QUEUE.put(msg);


Queues may be awaited using shared references:

        async fn foo() {
            ...
            let msg = QUEUE.block_on().await;
            ...
        }

Actors are mapped to hardware interrupts. Note that interrupt controller should be carefully adjusted to reflect IRQ:priority relations.
Actor's function is the infinite loop just like thread:

        async fn blinky() -> Infallible {
            let q = &QUEUE;
            loop {
                let msg = q.block_on().await;
                ...
            }
        }


Finally, there is executor (or scheduler) representing the set of ready-to-run actors:

        static SCHED: Executor = Executor::new();


It should be initialized once on startup and then called inside interrupt handlers designated to actor's execution:

        SCHED.schedule(...<the current interrupt vector number>...);


The main function should contain scheduler launch as its last statement:

        let mut future = blinky();
        ...

        SCHED.run({[
            (<vector number 0>, &mut future)
            ...
            (<vector number N>, &mut <future N>)
            ...
        ]});


Note that this function expects disabled interrupts and panics in case when they are enabled since this may lead to undefined behavior.

The framework also provides timer facility. All software timers are handled by single global timer object representing tick source.

        static TIMER: Timer< ...number of hierarchy levels ...> = Timer::new();


Timers form a hierarchy based on their timeout value. Default value is 10. After the global context is initialized in the main using init method:

        TIMER.init();


then some interrupt must provide periodic ticks using tick function:

        TIMER.tick();


After that any actor may use sleep_for method with given amount of ticks to sleep:

        loop {
            ...
            TIMER.sleep_for(10).await; // Sleep for 10 ticks.
        }


Tick duration is unspecified and depends on your settings.
That's it. See example folder for detailed descriptions.


Safety notes
------------

All the static-mut-objects are only accessed once during initialization, normal operational code of either actors or interrupts is safe and does not require unsafe code. Also the user is responsible for NVIC initialization and proper content of the vector table. 
Executor transmutes future objects and the array of active futures into static lifetime despite they're allocated on the stack, however, since 'run' function does
not return and all the actors are infinite loops this is considered safe.


Demo
----

The demos is a toy examples with just one actor which blinks the LED. Use 'make' to build.

