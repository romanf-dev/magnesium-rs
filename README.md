Simple hardware-assisted actor model implementation in Rust
===========================================================

Overview
--------

The framework relies on hardware features of the NVIC interrupt controller: the ability to set user-defined priorities to IRQs and possibility to trigger interrupts programmatically. Actor's priorities are mapped to interrupt vectors and interrupt controller acts as a hardware scheduler. Therefore, interrupts are used as 'execution engine' for actors with certain priority.

When some event (hardware interrupt) occurs, it allocates and posts the message to a queue, this causes activation of the subscribed actor, moving the message into its incoming mailbox, moving the actor to the list of ready ones and also triggering interrupt vector corresponding to actor's priority. The hardware automatically transfers control to the activated vector, its handler then calls framework's function 'schedule' which eventually calls activated actors.

Unlike the original version of the framework written in C, Rust version uses async functions and futures to make code more safe and readable.
Also, it uses no external crates except core:: so it may be helpful for those who want to dive into details.


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
            let msg = (&QUEUE).await;
            ...
        }


Actors are active objects representing computations over messages. They are declared with static lifetime as:

        static mut ACTOR: Actor = Actor::new(<priority>, <hardware IRQ number>);


Actors are mapped to hardware interrupts. Note that interrupt controller should be carefully adjusted to reflect IRQ:priority relations.
Each pair (actor: IRQn) must be unique (in other words, actors may share priority but must not share interrupt vectors).
Actor's function is the infinite loop just like thread:

        async fn blinky() -> Infallible {
            let q = &QUEUE;
            loop {
                let msg = q.await;
                ...
            }
        }


Finally, there is executor (or scheduler) representing the set of ready-to-run actors:

        static SCHED: Executor = Executor::new();


It should be called inside interrupt handlers designated to actor's execution:

        SCHED.schedule(...<the current interrupt vector number>...);


The main function should contain scheduler launch as its last statement:

        let mut future = blinky();
        ...

        SCHED.run(unsafe {[
            (&mut ACTOR, &mut future)
            ...
            (&mut <ACTOR N>, &mut <future N>)
            ...
        ]});


Note that this function expects disabled interrupts and panics in case when they are enabled since this may lead to undefined behavior.
That's it. See example folder for detailed descriptions.


Safety notes
------------

All the static-mut-objects are only accessed once during initialization, normal operational code of either actors or interrupts is safe and does not require unsafe code. Also the user is responsible for NVIC initialization and proper content of the vector table. 
Executor transmutes future objects and the array of active futures into static lifetime despite they're allocated on the stack, however, since 'run' function does
not return and all the actors are infinite loops this is considered safe.


Demo
----

The demo is a toy example with just one actor which blinks the LED on the STM32 Bluepill board (STM32F1 chip). Use 'make' to build.

