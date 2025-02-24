//! Swarms are a way to conveniently control many bots.
//!
//! See [`Swarm`] for more information.

mod chat;
mod events;
pub mod prelude;

use std::{
    collections::{HashMap, hash_map},
    future::Future,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{self, AtomicBool},
    },
    time::Duration,
};

use azalea_client::{
    Account, Client, DefaultPlugins, Event, JoinError, StartClientOpts, chat::ChatPacket,
    start_ecs_runner,
};
use azalea_protocol::{ServerAddress, resolver};
use azalea_world::InstanceContainer;
use bevy_app::{App, PluginGroup, PluginGroupBuilder, Plugins};
use bevy_ecs::{component::Component, entity::Entity, system::Resource, world::World};
use futures::future::{BoxFuture, join_all};
use parking_lot::{Mutex, RwLock};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::{BoxHandleFn, DefaultBotPlugins, HandleFn, JoinOpts, NoState, StartError};

/// A swarm is a way to conveniently control many bots at once, while also
/// being able to control bots at an individual level when desired.
///
/// Swarms are created from [`SwarmBuilder`].
///
/// Clients can be added to the swarm later via [`Swarm::add`], and can be
/// removed with [`Client::disconnect`].
#[derive(Clone, Resource)]
pub struct Swarm {
    pub ecs_lock: Arc<Mutex<World>>,

    bots: Arc<Mutex<HashMap<Entity, Client>>>,

    // the address is public and mutable so plugins can change it
    pub resolved_address: Arc<RwLock<SocketAddr>>,
    pub address: Arc<RwLock<ServerAddress>>,

    pub instance_container: Arc<RwLock<InstanceContainer>>,

    bots_tx: mpsc::UnboundedSender<(Option<Event>, Client)>,
    swarm_tx: mpsc::UnboundedSender<SwarmEvent>,

    run_schedule_sender: mpsc::Sender<()>,
}

/// Create a new [`Swarm`].
///
/// The generics of this struct stand for the following:
/// - S: State
/// - SS: Swarm State
/// - R: Return type of the handler
/// - SR: Return type of the swarm handler
///
/// You shouldn't have to manually set them though, they'll be inferred for you.
pub struct SwarmBuilder<S, SS, R, SR>
where
    S: Send + Sync + Clone + Component + 'static,
    SS: Default + Send + Sync + Clone + Resource + 'static,
{
    pub(crate) app: App,
    /// The accounts and proxies that are going to join the server.
    pub(crate) accounts: Vec<(Account, JoinOpts)>,
    /// The individual bot states. This must be the same length as `accounts`,
    /// since each bot gets one state.
    pub(crate) states: Vec<S>,
    /// The state for the overall swarm.
    pub(crate) swarm_state: SS,
    /// The function that's called every time a bot receives an [`Event`].
    pub(crate) handler: Option<BoxHandleFn<S, R>>,
    /// The function that's called every time the swarm receives a
    /// [`SwarmEvent`].
    pub(crate) swarm_handler: Option<BoxSwarmHandleFn<SS, SR>>,

    /// How long we should wait between each bot joining the server. Set to
    /// None to have every bot connect at the same time. None is different than
    /// a duration of 0, since if a duration is present the bots will wait for
    /// the previous one to be ready.
    pub(crate) join_delay: Option<std::time::Duration>,
}
impl SwarmBuilder<NoState, NoSwarmState, (), ()> {
    /// Start creating the swarm.
    #[must_use]
    pub fn new() -> Self {
        Self::new_without_plugins()
            .add_plugins(DefaultPlugins)
            .add_plugins(DefaultBotPlugins)
            .add_plugins(DefaultSwarmPlugins)
    }

    /// [`Self::new`] but without adding the plugins by default. This is useful
    /// if you want to disable a default plugin.
    ///
    /// You **must** add [`DefaultPlugins`], [`DefaultBotPlugins`], and
    /// [`DefaultSwarmPlugins`] to this.
    ///
    /// ```
    /// # use azalea::{prelude::*, swarm::prelude::*};
    /// use azalea::{app::PluginGroup, DefaultBotPlugins, DefaultPlugins, swarm::{DefaultSwarmPlugins}};
    /// use bevy_log::LogPlugin;
    ///
    /// let swarm_builder = SwarmBuilder::new_without_plugins()
    ///     .add_plugins(DefaultPlugins.build().disable::<LogPlugin>())
    ///     .add_plugins(DefaultBotPlugins)
    ///     .add_plugins(DefaultSwarmPlugins);
    /// # swarm_builder.set_handler(handle).set_swarm_handler(swarm_handle);
    /// # #[derive(Component, Resource, Clone, Default)]
    /// # pub struct State;
    /// # async fn handle(mut bot: Client, event: Event, state: State) -> anyhow::Result<()> {
    /// #     Ok(())
    /// # }
    /// # async fn swarm_handle(swarm: Swarm, event: SwarmEvent, state: State) -> anyhow::Result<()> {
    /// #     Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn new_without_plugins() -> Self {
        SwarmBuilder {
            // we create the app here so plugins can add onto it.
            // the schedules won't run until [`Self::start`] is called.
            app: App::new(),
            accounts: Vec::new(),
            states: Vec::new(),
            swarm_state: NoSwarmState,
            handler: None,
            swarm_handler: None,
            join_delay: None,
        }
    }
}

impl<SS, SR> SwarmBuilder<NoState, SS, (), SR>
where
    SS: Default + Send + Sync + Clone + Resource + 'static,
{
    /// Set the function that's called every time a bot receives an [`Event`].
    /// This is the way to handle normal per-bot events.
    ///
    /// Currently you can have up to one handler.
    ///
    /// ```
    /// # use azalea::{prelude::*, swarm::prelude::*};
    /// # let swarm_builder = SwarmBuilder::new().set_swarm_handler(swarm_handle);
    /// swarm_builder.set_handler(handle);
    ///
    /// #[derive(Component, Default, Clone)]
    /// struct State {}
    /// async fn handle(mut bot: Client, event: Event, state: State) -> anyhow::Result<()> {
    ///     Ok(())
    /// }
    ///
    /// # #[derive(Resource, Default, Clone)]
    /// # struct SwarmState {}
    /// # async fn swarm_handle(
    /// #     mut swarm: Swarm,
    /// #     event: SwarmEvent,
    /// #     state: SwarmState,
    /// # ) -> anyhow::Result<()> {
    /// #     Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn set_handler<S, Fut, R>(self, handler: HandleFn<S, Fut>) -> SwarmBuilder<S, SS, R, SR>
    where
        Fut: Future<Output = R> + Send + 'static,
        S: Send + Sync + Clone + Component + Default + 'static,
    {
        SwarmBuilder {
            handler: Some(Box::new(move |bot, event, state: S| {
                Box::pin(handler(bot, event, state))
            })),
            // if we added accounts before the State was set, we've gotta set it to the default now
            states: vec![S::default(); self.accounts.len()],
            app: self.app,
            ..self
        }
    }
}

impl<S, R> SwarmBuilder<S, NoSwarmState, R, ()>
where
    S: Send + Sync + Clone + Component + 'static,
{
    /// Set the function that's called every time the swarm receives a
    /// [`SwarmEvent`]. This is the way to handle global swarm events.
    ///
    /// Currently you can have up to one swarm handler.
    ///
    /// ```
    /// # use azalea::{prelude::*, swarm::prelude::*};
    /// # let swarm_builder = SwarmBuilder::new().set_handler(handle);
    /// swarm_builder.set_swarm_handler(swarm_handle);
    ///
    /// # #[derive(Component, Default, Clone)]
    /// # struct State {}
    ///
    /// # async fn handle(mut bot: Client, event: Event, state: State) -> anyhow::Result<()> {
    /// #     Ok(())
    /// # }
    ///
    /// #[derive(Resource, Default, Clone)]
    /// struct SwarmState {}
    /// async fn swarm_handle(
    ///     mut swarm: Swarm,
    ///     event: SwarmEvent,
    ///     state: SwarmState,
    /// ) -> anyhow::Result<()> {
    ///     Ok(())
    /// }
    /// ```
    #[must_use]
    pub fn set_swarm_handler<SS, Fut, SR>(
        self,
        handler: SwarmHandleFn<SS, Fut>,
    ) -> SwarmBuilder<S, SS, R, SR>
    where
        SS: Default + Send + Sync + Clone + Resource + 'static,
        Fut: Future<Output = SR> + Send + 'static,
    {
        SwarmBuilder {
            handler: self.handler,
            app: self.app,
            accounts: self.accounts,
            states: self.states,
            swarm_state: SS::default(),
            swarm_handler: Some(Box::new(move |swarm, event, state| {
                Box::pin(handler(swarm, event, state))
            })),
            join_delay: self.join_delay,
        }
    }
}

impl<S, SS, R, SR> SwarmBuilder<S, SS, R, SR>
where
    S: Send + Sync + Clone + Component + 'static,
    SS: Default + Send + Sync + Clone + Resource + 'static,
    R: Send + 'static,
    SR: Send + 'static,
{
    /// Add a vec of [`Account`]s to the swarm.
    ///
    /// Use [`Self::add_account`] to only add one account. If you want the
    /// clients to have different default states, add them one at a time with
    /// [`Self::add_account_with_state`].
    ///
    /// By default, every account will join at the same time, you can add a
    /// delay with [`Self::join_delay`].
    #[must_use]
    pub fn add_accounts(mut self, accounts: Vec<Account>) -> Self
    where
        S: Default,
    {
        for account in accounts {
            self = self.add_account(account);
        }

        self
    }

    /// Add a single new [`Account`] to the swarm. Use [`Self::add_accounts`] to
    /// add multiple accounts at a time.
    ///
    /// This will make the state for this client be the default, use
    /// [`Self::add_account_with_state`] to avoid that.
    #[must_use]
    pub fn add_account(self, account: Account) -> Self
    where
        S: Default,
    {
        self.add_account_with_state_and_opts(account, S::default(), JoinOpts::default())
    }

    /// Add an account with a custom initial state. Use just
    /// [`Self::add_account`] to use the Default implementation for the state.
    #[must_use]
    pub fn add_account_with_state(self, account: Account, state: S) -> Self {
        self.add_account_with_state_and_opts(account, state, JoinOpts::default())
    }

    /// Add an account with a custom initial state. Use just
    /// [`Self::add_account`] to use the Default implementation for the state.
    #[must_use]
    pub fn add_account_with_opts(self, account: Account, opts: JoinOpts) -> Self
    where
        S: Default,
    {
        self.add_account_with_state_and_opts(account, S::default(), opts)
    }

    /// Same as [`Self::add_account_with_state`], but allow passing in custom
    /// join options.
    #[must_use]
    pub fn add_account_with_state_and_opts(
        mut self,
        account: Account,
        state: S,
        join_opts: JoinOpts,
    ) -> Self {
        self.accounts.push((account, join_opts));
        self.states.push(state);
        self
    }

    /// Set the swarm state instead of initializing defaults.
    #[must_use]
    pub fn set_swarm_state(mut self, swarm_state: SS) -> Self {
        self.swarm_state = swarm_state;
        self
    }

    /// Add one or more plugins to this swarm.
    #[must_use]
    pub fn add_plugins<M>(mut self, plugins: impl Plugins<M>) -> Self {
        self.app.add_plugins(plugins);
        self
    }

    /// Set how long we should wait between each bot joining the server.
    ///
    /// By default, every bot will connect at the same time. If you set this
    /// field, however, the bots will wait for the previous one to have
    /// connected and *then* they'll wait the given duration.
    #[must_use]
    pub fn join_delay(mut self, delay: std::time::Duration) -> Self {
        self.join_delay = Some(delay);
        self
    }

    /// Build this `SwarmBuilder` into an actual [`Swarm`] and join the given
    /// server.
    ///
    /// The `address` argument can be a `&str`, [`ServerAddress`], or anything
    /// that implements `TryInto<ServerAddress>`.
    ///
    /// [`ServerAddress`]: azalea_protocol::ServerAddress
    pub async fn start(self, address: impl TryInto<ServerAddress>) -> Result<!, StartError> {
        // convert the TryInto<ServerAddress> into a ServerAddress
        let address: ServerAddress = match address.try_into() {
            Ok(address) => address,
            Err(_) => return Err(StartError::InvalidAddress),
        };

        self.start_with_default_opts(address, JoinOpts::default())
            .await
    }

    /// Do the same as [`Self::start`], but allow passing in default join
    /// options for the bots.
    pub async fn start_with_default_opts(
        self,
        address: impl TryInto<ServerAddress>,
        default_join_opts: JoinOpts,
    ) -> Result<!, StartError> {
        assert_eq!(
            self.accounts.len(),
            self.states.len(),
            "There must be exactly one state per bot."
        );

        debug!("Starting Azalea {}", env!("CARGO_PKG_VERSION"));

        // convert the TryInto<ServerAddress> into a ServerAddress
        let address = match address.try_into() {
            Ok(address) => address,
            Err(_) => return Err(StartError::InvalidAddress),
        };

        let address: ServerAddress = default_join_opts.custom_address.clone().unwrap_or(address);
        let resolved_address = if let Some(a) = default_join_opts.custom_resolved_address {
            a
        } else {
            resolver::resolve_address(&address).await?
        };

        let instance_container = Arc::new(RwLock::new(InstanceContainer::default()));

        // we can't modify the swarm plugins after this
        let (bots_tx, mut bots_rx) = mpsc::unbounded_channel();
        let (swarm_tx, mut swarm_rx) = mpsc::unbounded_channel();

        swarm_tx.send(SwarmEvent::Init).unwrap();

        let (run_schedule_sender, run_schedule_receiver) = mpsc::channel(1);

        let main_schedule_label = self.app.main().update_schedule.unwrap();

        let ecs_lock =
            start_ecs_runner(self.app, run_schedule_receiver, run_schedule_sender.clone());

        let swarm = Swarm {
            ecs_lock: ecs_lock.clone(),
            bots: Arc::new(Mutex::new(HashMap::new())),

            resolved_address: Arc::new(RwLock::new(resolved_address)),
            address: Arc::new(RwLock::new(address)),
            instance_container,

            bots_tx,

            swarm_tx: swarm_tx.clone(),

            run_schedule_sender,
        };

        // run the main schedule so the startup systems run
        {
            let mut ecs = ecs_lock.lock();
            ecs.insert_resource(swarm.clone());
            ecs.insert_resource(self.swarm_state.clone());
            ecs.run_schedule(main_schedule_label);
            ecs.clear_trackers();
        }

        // SwarmBuilder (self) isn't Send so we have to take all the things we need out
        // of it
        let swarm_clone = swarm.clone();
        let join_delay = self.join_delay;
        let accounts = self.accounts.clone();
        let states = self.states.clone();

        tokio::spawn(async move {
            if let Some(join_delay) = join_delay {
                // if there's a join delay, then join one by one
                for ((account, bot_join_opts), state) in accounts.iter().zip(states) {
                    let mut join_opts = default_join_opts.clone();
                    join_opts.update(bot_join_opts);
                    swarm_clone
                        .add_and_retry_forever_with_opts(account, state, &join_opts)
                        .await;
                    tokio::time::sleep(join_delay).await;
                }
            } else {
                // otherwise, join all at once
                let swarm_borrow = &swarm_clone;
                join_all(accounts.iter().zip(states).map(
                    |((account, bot_join_opts), state)| async {
                        let mut join_opts = default_join_opts.clone();
                        join_opts.update(bot_join_opts);
                        swarm_borrow
                            .clone()
                            .add_and_retry_forever_with_opts(account, state, &join_opts)
                            .await;
                    },
                ))
                .await;
            }

            swarm_tx.send(SwarmEvent::Login).unwrap();
        });

        let swarm_state = self.swarm_state;

        // Watch swarm_rx and send those events to the swarm_handle.
        let swarm_clone = swarm.clone();
        tokio::spawn(async move {
            while let Some(event) = swarm_rx.recv().await {
                if let Some(swarm_handler) = &self.swarm_handler {
                    tokio::spawn((swarm_handler)(
                        swarm_clone.clone(),
                        event,
                        swarm_state.clone(),
                    ));
                }
            }
        });

        // bot events
        while let Some((Some(first_event), first_bot)) = bots_rx.recv().await {
            if bots_rx.len() > 1_000 {
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, atomic::Ordering::Relaxed) {
                    warn!("the Client Event channel has more than 1000 items!")
                }
            }

            if let Some(handler) = &self.handler {
                let Some(first_bot_state) = first_bot.get_component::<S>() else {
                    error!(
                        "the first bot ({} / {}) is missing the required state component! none of the client handler functions will be called.",
                        first_bot.profile.name, first_bot.entity
                    );
                    continue;
                };
                let first_bot_entity = first_bot.entity;

                tokio::spawn((handler)(first_bot, first_event, first_bot_state.clone()));

                // this makes it not have to keep locking the ecs
                let mut states = HashMap::new();
                states.insert(first_bot_entity, first_bot_state);
                while let Ok((Some(event), bot)) = bots_rx.try_recv() {
                    let state = match states.entry(bot.entity) {
                        hash_map::Entry::Occupied(e) => e.into_mut(),
                        hash_map::Entry::Vacant(e) => {
                            let Some(state) = bot.get_component::<S>() else {
                                error!(
                                    "one of our bots ({} / {}) is missing the required state component! its client handler function will not be called.",
                                    bot.profile.name, bot.entity
                                );
                                continue;
                            };
                            e.insert(state)
                        }
                    };
                    tokio::spawn((handler)(bot, event, state.clone()));
                }
            }
        }

        unreachable!(
            "bots_rx.recv() should never be None because the bots_tx channel is never closed"
        );
    }
}

impl Default for SwarmBuilder<NoState, NoSwarmState, (), ()> {
    fn default() -> Self {
        Self::new()
    }
}

/// An event about something that doesn't have to do with a single bot.
#[derive(Clone, Debug)]
pub enum SwarmEvent {
    /// All the bots in the swarm have successfully joined the server.
    Login,
    /// The swarm was created. This is only fired once, and it's guaranteed to
    /// be the first event to fire.
    Init,
    /// A bot got disconnected from the server.
    ///
    /// You can implement an auto-reconnect by calling [`Swarm::add_with_opts`]
    /// with the account and options from this event.
    Disconnect(Box<Account>, JoinOpts),
    /// At least one bot received a chat message.
    Chat(ChatPacket),
}

pub type SwarmHandleFn<SS, Fut> = fn(Swarm, SwarmEvent, SS) -> Fut;
pub type BoxSwarmHandleFn<SS, R> =
    Box<dyn Fn(Swarm, SwarmEvent, SS) -> BoxFuture<'static, R> + Send>;

/// Make a bot [`Swarm`].
///
/// [`Swarm`]: struct.Swarm.html
///
/// # Examples
/// ```rust,no_run
/// use azalea::{prelude::*, swarm::prelude::*};
/// use std::time::Duration;
///
/// #[derive(Default, Clone, Component)]
/// struct State {}
///
/// #[derive(Default, Clone, Resource)]
/// struct SwarmState {}
///
/// #[tokio::main]
/// async fn main() {
///     let mut accounts = Vec::new();
///     let mut states = Vec::new();
///
///     for i in 0..10 {
///         accounts.push(Account::offline(&format!("bot{i}")));
///         states.push(State::default());
///     }
///
///     SwarmBuilder::new()
///         .add_accounts(accounts.clone())
///         .set_handler(handle)
///         .set_swarm_handler(swarm_handle)
///         .join_delay(Duration::from_millis(1000))
///         .start("localhost")
///         .await
///         .unwrap();
/// }
///
/// async fn handle(bot: Client, event: Event, _state: State) -> anyhow::Result<()> {
///     match &event {
///         _ => {}
///     }
///     Ok(())
/// }
///
/// async fn swarm_handle(
///     mut swarm: Swarm,
///     event: SwarmEvent,
///     _state: SwarmState,
/// ) -> anyhow::Result<()> {
///     match &event {
///         SwarmEvent::Disconnect(account, join_opts) => {
///             // automatically reconnect after 5 seconds
///             tokio::time::sleep(Duration::from_secs(5)).await;
///             swarm.add_with_opts(account, State::default(), join_opts).await?;
///         }
///         SwarmEvent::Chat(m) => {
///             println!("{}", m.message().to_ansi());
///         }
///         _ => {}
///     }
///     Ok(())
/// }
impl Swarm {
    /// Add a new account to the swarm. You can remove it later by calling
    /// [`Client::disconnect`].
    ///
    /// # Errors
    ///
    /// Returns an `Err` if the bot could not do a handshake successfully.
    pub async fn add<S: Component + Clone>(
        &mut self,
        account: &Account,
        state: S,
    ) -> Result<Client, JoinError> {
        self.add_with_opts(account, state, &JoinOpts::default())
            .await
    }
    /// Add a new account to the swarm, using custom options. This is useful if
    /// you want bots in the same swarm to connect to different addresses.
    /// Usually you'll just want [`Self::add`] though.
    ///
    /// # Errors
    ///
    /// Returns an `Err` if the bot could not do a handshake successfully.
    pub async fn add_with_opts<S: Component + Clone>(
        &self,
        account: &Account,
        state: S,
        join_opts: &JoinOpts,
    ) -> Result<Client, JoinError> {
        debug!("add_with_opts called for account {}", account.username);

        let address = join_opts
            .custom_address
            .clone()
            .unwrap_or_else(|| self.address.read().clone());
        let resolved_address = join_opts
            .custom_resolved_address
            .unwrap_or_else(|| *self.resolved_address.read());

        let (bot, rx) = Client::start_client(StartClientOpts {
            ecs_lock: self.ecs_lock.clone(),
            account,
            address: &address,
            resolved_address: &resolved_address,
            proxy: join_opts.proxy.clone(),
            run_schedule_sender: self.run_schedule_sender.clone(),
        })
        .await?;
        // add the state to the client
        {
            let mut ecs = self.ecs_lock.lock();
            ecs.entity_mut(bot.entity).insert(state);
        }

        self.bots.lock().insert(bot.entity, bot.clone());

        let cloned_bots = self.bots.clone();
        let cloned_bots_tx = self.bots_tx.clone();
        let cloned_bot = bot.clone();
        let swarm_tx = self.swarm_tx.clone();
        let join_opts = join_opts.clone();
        tokio::spawn(Self::event_copying_task(
            rx,
            cloned_bots,
            cloned_bots_tx,
            cloned_bot,
            swarm_tx,
            join_opts,
        ));

        Ok(bot)
    }

    async fn event_copying_task(
        mut rx: mpsc::UnboundedReceiver<Event>,
        cloned_bots: Arc<Mutex<HashMap<Entity, Client>>>,
        cloned_bots_tx: mpsc::UnboundedSender<(Option<Event>, Client)>,
        cloned_bot: Client,
        swarm_tx: mpsc::UnboundedSender<SwarmEvent>,
        join_opts: JoinOpts,
    ) {
        while let Some(event) = rx.recv().await {
            if rx.len() > 1_000 {
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, atomic::Ordering::Relaxed) {
                    warn!("the client's Event channel has more than 1000 items!")
                }
            }

            // we can't handle events here (since we can't copy the handler),
            // they're handled above in SwarmBuilder::start
            if let Err(e) = cloned_bots_tx.send((Some(event), cloned_bot.clone())) {
                error!("Error sending event to swarm: {e}");
            }
        }
        debug!("client sender ended, removing from cloned_bots and sending SwarmEvent::Disconnect");

        cloned_bots.lock().remove(&cloned_bot.entity);
        let account = cloned_bot
            .get_component::<Account>()
            .expect("bot is missing required Account component");
        swarm_tx
            .send(SwarmEvent::Disconnect(Box::new(account), join_opts))
            .unwrap();
    }

    /// Add a new account to the swarm, retrying if it couldn't join. This will
    /// run forever until the bot joins or the task is aborted.
    ///
    /// This does exponential backoff (though very limited), starting at 5
    /// seconds and doubling up to 15 seconds.
    pub async fn add_and_retry_forever<S: Component + Clone>(
        &self,
        account: &Account,
        state: S,
    ) -> Client {
        self.add_and_retry_forever_with_opts(account, state, &JoinOpts::default())
            .await
    }

    /// Same as [`Self::add_and_retry_forever`], but allow passing custom join
    /// options.
    pub async fn add_and_retry_forever_with_opts<S: Component + Clone>(
        &self,
        account: &Account,
        state: S,
        opts: &JoinOpts,
    ) -> Client {
        let mut disconnects = 0;
        loop {
            match self.add_with_opts(account, state.clone(), opts).await {
                Ok(bot) => return bot,
                Err(e) => {
                    disconnects += 1;
                    let delay = (Duration::from_secs(5) * 2u32.pow(disconnects.min(16)))
                        .min(Duration::from_secs(15));
                    let username = account.username.clone();

                    match &e {
                        JoinError::Disconnect { reason } => {
                            error!(
                                "Error joining as {username}, server says: \"{reason}\". Waiting {delay:?} and trying again."
                            );
                        }
                        _ => {
                            error!(
                                "Error joining as {username}: {e}. Waiting {delay:?} and trying again."
                            );
                        }
                    }

                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

impl IntoIterator for Swarm {
    type Item = Client;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    /// Iterate over the bots in this swarm.
    ///
    /// ```rust,no_run
    /// # use azalea::{prelude::*, swarm::prelude::*};
    /// #[derive(Component, Clone)]
    /// # pub struct State;
    /// # fn example(swarm: Swarm) {
    /// for bot in swarm {
    ///     let state = bot.component::<State>();
    ///     // ...
    /// }
    /// # }
    /// ```
    fn into_iter(self) -> Self::IntoIter {
        self.bots
            .lock()
            .clone()
            .into_values()
            .collect::<Vec<_>>()
            .into_iter()
    }
}

/// This plugin group will add all the default plugins necessary for swarms to
/// work.
pub struct DefaultSwarmPlugins;

impl PluginGroup for DefaultSwarmPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(chat::SwarmChatPlugin)
            .add(events::SwarmPlugin)
    }
}

/// A marker that can be used in place of a SwarmState in [`SwarmBuilder`]. You
/// probably don't need to use this manually since the compiler will infer it
/// for you.
#[derive(Resource, Clone, Default)]
pub struct NoSwarmState;
