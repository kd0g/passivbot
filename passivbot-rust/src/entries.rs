use crate::types::{
    BotParams, ExchangeParams, Order, OrderType, Position, StateParams,
    TrailingPriceBundle, DivergenceBundle,
};
use crate::utils::{
    calc_ema_price_ask, calc_ema_price_bid, calc_new_psize_pprice, calc_wallet_exposure,
    calc_wallet_exposure_if_filled, cost_to_qty, interpolate, round_, round_dn, round_up,
};

pub fn calc_initial_entry_qty(
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
    balance: f64,
    entry_price: f64,
) -> f64 {
    f64::max(
        calc_min_entry_qty(entry_price, &exchange_params),
        round_(
            cost_to_qty(
                balance * bot_params.wallet_exposure_limit * bot_params.entry_initial_qty_pct,
                entry_price,
                exchange_params.c_mult,
            ),
            exchange_params.qty_step,
        ),
    )
}

pub fn calc_min_entry_qty(entry_price: f64, exchange_params: &ExchangeParams) -> f64 {
    f64::max(
        exchange_params.min_qty,
        round_up(
            cost_to_qty(
                exchange_params.min_cost,
                entry_price,
                exchange_params.c_mult,
            ),
            exchange_params.qty_step,
        ),
    )
}

pub fn calc_cropped_reentry_qty(
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
    position: &Position,
    wallet_exposure: f64,
    balance: f64,
    entry_qty: f64,
    entry_price: f64,
) -> (f64, f64) {
    let position_size_abs = position.size.abs();
    let entry_qty_abs = entry_qty.abs();
    let wallet_exposure_if_filled = calc_wallet_exposure_if_filled(
        balance,
        position_size_abs,
        position.price,
        entry_qty_abs,
        entry_price,
        &exchange_params,
    );
    let min_entry_qty = calc_min_entry_qty(entry_price, &exchange_params);
    if wallet_exposure_if_filled > bot_params.wallet_exposure_limit * 1.01 {
        // reentry too big. Crop current reentry qty.
        let entry_qty_abs = interpolate(
            bot_params.wallet_exposure_limit,
            &[wallet_exposure, wallet_exposure_if_filled],
            &[position_size_abs, position_size_abs + entry_qty_abs],
        ) - position_size_abs;
        (
            wallet_exposure_if_filled,
            f64::max(
                round_(entry_qty_abs, exchange_params.qty_step),
                min_entry_qty,
            ),
        )
    } else {
        (
            wallet_exposure_if_filled,
            f64::max(entry_qty_abs, min_entry_qty),
        )
    }
}

pub fn calc_reentry_qty(
    entry_price: f64,
    balance: f64,
    position_size: f64,
    double_down_factor: f64,
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
) -> f64 {
    f64::max(
        calc_min_entry_qty(entry_price, &exchange_params),
        round_(
            f64::max(
                position_size.abs() * double_down_factor,
                cost_to_qty(balance, entry_price, exchange_params.c_mult)
                    * bot_params.wallet_exposure_limit
                    * bot_params.entry_initial_qty_pct,
            ),
            exchange_params.qty_step,
        ),
    )
}

fn calc_reentry_price_bid(
    position_price: f64,
    wallet_exposure: f64,
    order_book_bid: f64,
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
) -> f64 {
    let multiplier =
        (wallet_exposure / bot_params.wallet_exposure_limit) * bot_params.entry_grid_spacing_weight;
    let reentry_price = f64::min(
        round_dn(
            position_price * (1.0 - bot_params.entry_grid_spacing_pct * (1.0 + multiplier)),
            exchange_params.price_step,
        ),
        order_book_bid,
    );
    if reentry_price <= exchange_params.price_step {
        0.0
    } else {
        reentry_price
    }
}

fn calc_reentry_price_ask(
    position_price: f64,
    wallet_exposure: f64,
    order_book_ask: f64,
    exchange_params: &ExchangeParams,
    bot_params: &BotParams,
) -> f64 {
    let multiplier =
        (wallet_exposure / bot_params.wallet_exposure_limit) * bot_params.entry_grid_spacing_weight;
    let reentry_price = f64::max(
        round_up(
            position_price * (1.0 + bot_params.entry_grid_spacing_pct * (1.0 + multiplier)),
            exchange_params.price_step,
        ),
        order_book_ask,
    );
    if reentry_price <= exchange_params.price_step {
        0.0
    } else {
        reentry_price
    }
}

pub fn calc_grid_entry_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
) -> Order {
    if bot_params.wallet_exposure_limit == 0.0 || state_params.balance <= 0.0 {
        return Order::default();
    }
    let initial_entry_price = calc_ema_price_bid(
        exchange_params.price_step,
        state_params.order_book.bid,
        state_params.ema_bands.lower,
        bot_params.entry_initial_ema_dist,
    );
    if initial_entry_price <= exchange_params.price_step {
        return Order::default();
    }
    let initial_entry_qty = calc_initial_entry_qty(
        exchange_params,
        bot_params,
        state_params.balance,
        initial_entry_price,
    );
    if position.size == 0.0 {
        return Order {
            qty: initial_entry_qty,
            price: initial_entry_price,
            order_type: OrderType::EntryInitialNormalLong,
        };
    } else if position.size < initial_entry_qty * 0.8 {
        return Order {
            qty: f64::max(
                calc_min_entry_qty(initial_entry_price, &exchange_params),
                round_dn(initial_entry_qty - position.size, exchange_params.qty_step),
            ),
            price: initial_entry_price,
            order_type: OrderType::EntryInitialPartialLong,
        };
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position.size,
        position.price,
    );
    if wallet_exposure >= bot_params.wallet_exposure_limit * 0.999 {
        return Order::default();
    }

    // normal re-entry
    let reentry_price = calc_reentry_price_bid(
        position.price,
        wallet_exposure,
        state_params.order_book.bid,
        exchange_params,
        bot_params,
    );
    if reentry_price <= 0.0 {
        return Order::default();
    }
    let reentry_qty = f64::max(
        calc_reentry_qty(
            reentry_price,
            state_params.balance,
            position.size,
            bot_params.entry_grid_double_down_factor,
            exchange_params,
            bot_params,
        ),
        initial_entry_qty,
    );
    let (wallet_exposure_if_filled, reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        position,
        wallet_exposure,
        state_params.balance,
        reentry_qty,
        reentry_price,
    );
    if reentry_qty_cropped < reentry_qty {
        return Order {
            qty: reentry_qty_cropped,
            price: reentry_price,
            order_type: OrderType::EntryGridCroppedLong,
        };
    }
    // preview next order to check if reentry qty is to be inflated
    let (psize_if_filled, pprice_if_filled) = calc_new_psize_pprice(
        position.size,
        position.price,
        reentry_qty,
        reentry_price,
        exchange_params.qty_step,
    );
    let next_reentry_price = calc_reentry_price_bid(
        pprice_if_filled,
        wallet_exposure_if_filled,
        state_params.order_book.bid,
        exchange_params,
        bot_params,
    );
    let next_reentry_qty = f64::max(
        calc_reentry_qty(
            next_reentry_price,
            state_params.balance,
            psize_if_filled,
            bot_params.entry_grid_double_down_factor,
            exchange_params,
            bot_params,
        ),
        initial_entry_qty,
    );
    let (next_wallet_exposure_if_filled, next_reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        &Position {
            size: psize_if_filled,
            price: pprice_if_filled,
        },
        wallet_exposure_if_filled,
        state_params.balance,
        next_reentry_qty,
        next_reentry_price,
    );
    let effective_double_down_factor = next_reentry_qty_cropped / psize_if_filled;
    if effective_double_down_factor < bot_params.entry_grid_double_down_factor * 0.25 {
        // next reentry too small. Inflate current reentry.
        let new_entry_qty = interpolate(
            bot_params.wallet_exposure_limit,
            &[wallet_exposure, wallet_exposure_if_filled],
            &[position.size, position.size + reentry_qty],
        ) - position.size;
        Order {
            qty: round_(new_entry_qty, exchange_params.qty_step),
            price: reentry_price,
            order_type: OrderType::EntryGridInflatedLong,
        }
    } else {
        Order {
            qty: reentry_qty,
            price: reentry_price,
            order_type: OrderType::EntryGridNormalLong,
        }
    }
}

pub fn calc_next_entry_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
    divergence_bundle: &DivergenceBundle,
) -> Order {
    // determines whether trailing or grid order, returns Order
    if bot_params.wallet_exposure_limit == 0.0 || state_params.balance <= 0.0 {
        // no orders
        return Order::default();
    }
    if bot_params.entry_trailing_grid_ratio >= 1.0 || bot_params.entry_trailing_grid_ratio <= -1.0 {
        // return trailing only
        return if bot_params.enable_divergence_entry {
            calc_divergence_entry_long(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                &divergence_bundle,
            )
        } else {
            calc_trailing_entry_long(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                &trailing_price_bundle,
            )
        };
    } else if bot_params.entry_trailing_grid_ratio == 0.0 {
        // return grid only
        return calc_grid_entry_long(&exchange_params, &state_params, &bot_params, &position);
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position.size,
        position.price,
    );
    let wallet_exposure_ratio = wallet_exposure / bot_params.wallet_exposure_limit;
    if bot_params.entry_trailing_grid_ratio > 0.0 {
        // trailing first
        if wallet_exposure_ratio < bot_params.entry_trailing_grid_ratio {
            // return trailing order, but crop to max bot_params.wallet_exposure_limit * bot_params.entry_trailing_grid_ratio + 1%
            if wallet_exposure == 0.0 {
                if bot_params.enable_divergence_entry {
                    calc_divergence_entry_long(
                        &exchange_params,
                        &state_params,
                        &bot_params,
                        &position,
                        &divergence_bundle,
                    )
                } else {
                    calc_trailing_entry_long(
                        &exchange_params,
                        &state_params,
                        &bot_params,
                        &position,
                        &trailing_price_bundle,
                    )
                }
            } else {
                let mut bot_params_modified = bot_params.clone();
                bot_params_modified.wallet_exposure_limit =
                    bot_params.wallet_exposure_limit * bot_params.entry_trailing_grid_ratio * 1.01;
                if bot_params.enable_divergence_entry {
                    calc_divergence_entry_long(
                        &exchange_params,
                        &state_params,
                        &bot_params_modified,
                        &position,
                        &divergence_bundle,
                    )
                } else {
                    calc_trailing_entry_long(
                        &exchange_params,
                        &state_params,
                        &bot_params_modified,
                        &position,
                        &trailing_price_bundle,
                    )
                }
            }
        } else {
            // return grid order
            calc_grid_entry_long(&exchange_params, &state_params, &bot_params, &position)
        }
    } else {
        // grid first
        if wallet_exposure_ratio < 1.0 + bot_params.entry_trailing_grid_ratio {
            // return grid order, but crop to max bot_params.wallet_exposure_limit * (1.0 + bot_params.entry_trailing_grid_ratio) + 1%
            if wallet_exposure == 0.0 {
                calc_grid_entry_long(&exchange_params, &state_params, &bot_params, &position)
            } else {
                let mut bot_params_modified = bot_params.clone();
                if wallet_exposure != 0.0 {
                    bot_params_modified.wallet_exposure_limit = bot_params.wallet_exposure_limit
                        * (1.0 + bot_params.entry_trailing_grid_ratio)
                        * 1.01;
                }
                calc_grid_entry_long(
                    &exchange_params,
                    &state_params,
                    &bot_params_modified,
                    &position,
                )
            }
        } else {
            if bot_params.enable_divergence_entry {
                calc_divergence_entry_long(
                    &exchange_params,
                    &state_params,
                    &bot_params,
                    &position,
                    &divergence_bundle,
                )
            } else {
                calc_trailing_entry_long(
                    &exchange_params,
                    &state_params,
                    &bot_params,
                    &position,
                    &trailing_price_bundle,
                )
            }
        }
    }
}

pub fn calc_trailing_entry_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
) -> Order {
    let initial_entry_price = calc_ema_price_bid(
        exchange_params.price_step,
        state_params.order_book.bid,
        state_params.ema_bands.lower,
        bot_params.entry_initial_ema_dist,
    );
    if initial_entry_price <= exchange_params.price_step {
        return Order::default();
    }
    let initial_entry_qty = calc_initial_entry_qty(
        exchange_params,
        bot_params,
        state_params.balance,
        initial_entry_price,
    );
    if position.size == 0.0 {
        // normal initial entry
        return Order {
            qty: initial_entry_qty,
            price: initial_entry_price,
            order_type: OrderType::EntryInitialNormalLong,
        };
    } else if position.size < initial_entry_qty * 0.8 {
        return Order {
            qty: f64::max(
                calc_min_entry_qty(initial_entry_price, &exchange_params),
                round_dn(initial_entry_qty - position.size, exchange_params.qty_step),
            ),
            price: initial_entry_price,
            order_type: OrderType::EntryInitialPartialLong,
        };
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position.size,
        position.price,
    );
    if wallet_exposure > bot_params.wallet_exposure_limit * 0.999 {
        return Order::default();
    }
    let mut entry_triggered = false;
    let mut reentry_price = 0.0;
    if bot_params.entry_trailing_threshold_pct <= 0.0 {
        // means trailing entry immediately from pos change
        if bot_params.entry_trailing_retracement_pct > 0.0
            && trailing_price_bundle.max_since_min
                > trailing_price_bundle.min_since_open
                    * (1.0 + bot_params.entry_trailing_retracement_pct)
        {
            entry_triggered = true;
            reentry_price = state_params.order_book.bid;
        }
    } else {
        // means trailing entry will activate only after a threshold
        if bot_params.entry_trailing_retracement_pct <= 0.0 {
            // close at threshold
            entry_triggered = true;
            reentry_price = f64::min(
                state_params.order_book.bid,
                round_dn(
                    position.price * (1.0 - bot_params.entry_trailing_threshold_pct),
                    exchange_params.price_step,
                ),
            );
        } else {
            // enter if both conditions are met
            if trailing_price_bundle.min_since_open
                < position.price * (1.0 - bot_params.entry_trailing_threshold_pct)
                && trailing_price_bundle.max_since_min
                    > trailing_price_bundle.min_since_open
                        * (1.0 + bot_params.entry_trailing_retracement_pct)
            {
                entry_triggered = true;
                reentry_price = f64::min(
                    state_params.order_book.bid,
                    round_dn(
                        position.price
                            * (1.0 - bot_params.entry_trailing_threshold_pct
                                + bot_params.entry_trailing_retracement_pct),
                        exchange_params.price_step,
                    ),
                );
            }
        }
    }
    if !entry_triggered {
        return Order {
            qty: 0.0,
            price: 0.0,
            order_type: OrderType::EntryTrailingNormalLong,
        };
    }
    let reentry_qty = f64::max(
        calc_reentry_qty(
            reentry_price,
            state_params.balance,
            position.size,
            bot_params.entry_trailing_double_down_factor,
            &exchange_params,
            &bot_params,
        ),
        initial_entry_qty,
    );
    let (wallet_exposure_if_filled, reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        position,
        wallet_exposure,
        state_params.balance,
        reentry_qty,
        reentry_price,
    );
    if reentry_qty_cropped < reentry_qty {
        Order {
            qty: reentry_qty_cropped,
            price: reentry_price,
            order_type: OrderType::EntryTrailingCroppedLong,
        }
    } else {
        Order {
            qty: reentry_qty,
            price: reentry_price,
            order_type: OrderType::EntryTrailingNormalLong,
        }
    }
}

pub fn calc_divergence_entry_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    divergence_bundle: &DivergenceBundle,
) -> Order {
    if !bot_params.enable_divergence_entry {
        return Order::default();
    }
    if divergence_bundle.curr_low < divergence_bundle.prev_low
        && divergence_bundle.curr_rsi
            > divergence_bundle.prev_rsi + bot_params.divergence_rsi_tolerance
    {
        let entry_price = state_params.order_book.bid;
        let qty = calc_min_entry_qty(entry_price, exchange_params);
        return Order {
            qty,
            price: entry_price,
            order_type: OrderType::EntryDivergenceLong,
        };
    }
    Order::default()
}

pub fn calc_grid_entry_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
) -> Order {
    if bot_params.wallet_exposure_limit == 0.0 || state_params.balance <= 0.0 {
        return Order::default();
    }
    let initial_entry_price = calc_ema_price_ask(
        exchange_params.price_step,
        state_params.order_book.ask,
        state_params.ema_bands.upper,
        bot_params.entry_initial_ema_dist,
    );
    if initial_entry_price <= exchange_params.price_step {
        return Order::default();
    }
    let initial_entry_qty = calc_initial_entry_qty(
        exchange_params,
        bot_params,
        state_params.balance,
        initial_entry_price,
    );
    let position_size_abs = position.size.abs();
    if position_size_abs == 0.0 {
        return Order {
            qty: -initial_entry_qty,
            price: initial_entry_price,
            order_type: OrderType::EntryInitialNormalShort,
        };
    } else if position_size_abs < initial_entry_qty * 0.8 {
        return Order {
            qty: -f64::max(
                calc_min_entry_qty(initial_entry_price, &exchange_params),
                round_dn(
                    initial_entry_qty - position_size_abs,
                    exchange_params.qty_step,
                ),
            ),
            price: initial_entry_price,
            order_type: OrderType::EntryInitialPartialShort,
        };
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position_size_abs,
        position.price,
    );
    if wallet_exposure >= bot_params.wallet_exposure_limit * 0.999 {
        return Order::default();
    }

    // normal re-entry
    let reentry_price = calc_reentry_price_ask(
        position.price,
        wallet_exposure,
        state_params.order_book.ask,
        exchange_params,
        bot_params,
    );
    if reentry_price <= 0.0 {
        return Order::default();
    }
    let reentry_qty = f64::max(
        calc_reentry_qty(
            reentry_price,
            state_params.balance,
            position_size_abs,
            bot_params.entry_grid_double_down_factor,
            exchange_params,
            bot_params,
        ),
        initial_entry_qty,
    );
    let (wallet_exposure_if_filled, reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        position,
        wallet_exposure,
        state_params.balance,
        reentry_qty,
        reentry_price,
    );
    if reentry_qty_cropped < reentry_qty {
        return Order {
            qty: -reentry_qty_cropped,
            price: reentry_price,
            order_type: OrderType::EntryGridCroppedShort,
        };
    }
    // preview next order to check if reentry qty is to be inflated
    let (psize_if_filled, pprice_if_filled) = calc_new_psize_pprice(
        position_size_abs,
        position.price,
        reentry_qty,
        reentry_price,
        exchange_params.qty_step,
    );
    let next_reentry_price = calc_reentry_price_ask(
        pprice_if_filled,
        wallet_exposure_if_filled,
        state_params.order_book.ask,
        exchange_params,
        bot_params,
    );
    let next_reentry_qty = f64::max(
        calc_reentry_qty(
            next_reentry_price,
            state_params.balance,
            psize_if_filled,
            bot_params.entry_grid_double_down_factor,
            exchange_params,
            bot_params,
        ),
        initial_entry_qty,
    );
    let (next_wallet_exposure_if_filled, next_reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        &Position {
            size: psize_if_filled,
            price: pprice_if_filled,
        },
        wallet_exposure_if_filled,
        state_params.balance,
        next_reentry_qty,
        next_reentry_price,
    );
    let effective_double_down_factor = next_reentry_qty_cropped / psize_if_filled;
    if effective_double_down_factor < bot_params.entry_grid_double_down_factor * 0.25 {
        // next reentry too small. Inflate current reentry.
        let new_entry_qty = interpolate(
            bot_params.wallet_exposure_limit,
            &[wallet_exposure, wallet_exposure_if_filled],
            &[position_size_abs, position_size_abs + reentry_qty],
        ) - position_size_abs;
        Order {
            qty: -round_(new_entry_qty, exchange_params.qty_step),
            price: reentry_price,
            order_type: OrderType::EntryGridInflatedShort,
        }
    } else {
        Order {
            qty: -reentry_qty,
            price: reentry_price,
            order_type: OrderType::EntryGridNormalShort,
        }
    }
}

pub fn calc_trailing_entry_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
) -> Order {
    let initial_entry_price = calc_ema_price_ask(
        exchange_params.price_step,
        state_params.order_book.ask,
        state_params.ema_bands.upper,
        bot_params.entry_initial_ema_dist,
    );
    if initial_entry_price <= exchange_params.price_step {
        return Order::default();
    }
    let initial_entry_qty = calc_initial_entry_qty(
        exchange_params,
        bot_params,
        state_params.balance,
        initial_entry_price,
    );
    let position_size_abs = position.size.abs();
    if position_size_abs == 0.0 {
        // normal initial entry
        return Order {
            qty: -initial_entry_qty,
            price: initial_entry_price,
            order_type: OrderType::EntryInitialNormalShort,
        };
    } else if position_size_abs < initial_entry_qty * 0.8 {
        return Order {
            qty: -f64::max(
                calc_min_entry_qty(initial_entry_price, &exchange_params),
                round_dn(
                    initial_entry_qty - position_size_abs,
                    exchange_params.qty_step,
                ),
            ),
            price: initial_entry_price,
            order_type: OrderType::EntryInitialPartialShort,
        };
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position_size_abs,
        position.price,
    );
    if wallet_exposure > bot_params.wallet_exposure_limit * 0.999 {
        return Order::default();
    }
    let mut entry_triggered = false;
    let mut reentry_price = 0.0;
    if bot_params.entry_trailing_threshold_pct <= 0.0 {
        // means trailing entry immediately from pos change
        if bot_params.entry_trailing_retracement_pct > 0.0
            && trailing_price_bundle.min_since_max
                < trailing_price_bundle.max_since_open
                    * (1.0 - bot_params.entry_trailing_retracement_pct)
        {
            entry_triggered = true;
            reentry_price = state_params.order_book.ask;
        }
    } else {
        // means trailing entry will activate only after a threshold
        if bot_params.entry_trailing_retracement_pct <= 0.0 {
            // enter at threshold
            entry_triggered = true;
            reentry_price = f64::max(
                state_params.order_book.ask,
                round_up(
                    position.price * (1.0 + bot_params.entry_trailing_threshold_pct),
                    exchange_params.price_step,
                ),
            );
        } else {
            // enter if both conditions are met
            if trailing_price_bundle.max_since_open
                > position.price * (1.0 + bot_params.entry_trailing_threshold_pct)
                && trailing_price_bundle.min_since_max
                    < trailing_price_bundle.max_since_open
                        * (1.0 - bot_params.entry_trailing_retracement_pct)
            {
                entry_triggered = true;
                reentry_price = f64::max(
                    state_params.order_book.ask,
                    round_up(
                        position.price
                            * (1.0 + bot_params.entry_trailing_threshold_pct
                                - bot_params.entry_trailing_retracement_pct),
                        exchange_params.price_step,
                    ),
                );
            }
        }
    }
    if !entry_triggered {
        return Order {
            qty: 0.0,
            price: 0.0,
            order_type: OrderType::EntryTrailingNormalShort,
        };
    }
    let reentry_qty = f64::max(
        calc_reentry_qty(
            reentry_price,
            state_params.balance,
            position_size_abs,
            bot_params.entry_trailing_double_down_factor,
            &exchange_params,
            &bot_params,
        ),
        initial_entry_qty,
    );
    let (wallet_exposure_if_filled, reentry_qty_cropped) = calc_cropped_reentry_qty(
        exchange_params,
        bot_params,
        position,
        wallet_exposure,
        state_params.balance,
        reentry_qty,
        reentry_price,
    );
    if reentry_qty_cropped < reentry_qty {
        Order {
            qty: -reentry_qty_cropped,
            price: reentry_price,
            order_type: OrderType::EntryTrailingCroppedShort,
        }
    } else {
        Order {
            qty: -reentry_qty,
            price: reentry_price,
            order_type: OrderType::EntryTrailingNormalShort,
        }
    }
}

pub fn calc_next_entry_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
) -> Order {
    // determines whether trailing or grid order, returns Order
    if bot_params.wallet_exposure_limit == 0.0 || state_params.balance <= 0.0 {
        // no orders
        return Order::default();
    }
    if bot_params.entry_trailing_grid_ratio >= 1.0 || bot_params.entry_trailing_grid_ratio <= -1.0 {
        // return trailing only
        return calc_trailing_entry_short(
            &exchange_params,
            &state_params,
            &bot_params,
            &position,
            &trailing_price_bundle,
        );
    } else if bot_params.entry_trailing_grid_ratio == 0.0 {
        // return grid only
        return calc_grid_entry_short(&exchange_params, &state_params, &bot_params, &position);
    }
    let wallet_exposure = calc_wallet_exposure(
        exchange_params.c_mult,
        state_params.balance,
        position.size.abs(),
        position.price,
    );
    let wallet_exposure_ratio = wallet_exposure / bot_params.wallet_exposure_limit;
    if bot_params.entry_trailing_grid_ratio > 0.0 {
        // trailing first
        if wallet_exposure_ratio < bot_params.entry_trailing_grid_ratio {
            if wallet_exposure == 0.0 {
                calc_trailing_entry_short(
                    &exchange_params,
                    &state_params,
                    &bot_params,
                    &position,
                    &trailing_price_bundle,
                )
            } else {
                // return trailing order, but crop to max bot_params.wallet_exposure_limit * bot_params.entry_trailing_grid_ratio + 1%
                let mut bot_params_modified = bot_params.clone();
                bot_params_modified.wallet_exposure_limit =
                    bot_params.wallet_exposure_limit * bot_params.entry_trailing_grid_ratio * 1.01;
                calc_trailing_entry_short(
                    &exchange_params,
                    &state_params,
                    &bot_params_modified,
                    &position,
                    &trailing_price_bundle,
                )
            }
        } else {
            // return grid order
            calc_grid_entry_short(&exchange_params, &state_params, &bot_params, &position)
        }
    } else {
        // grid first
        if wallet_exposure_ratio < 1.0 + bot_params.entry_trailing_grid_ratio {
            // return grid order, but crop to max bot_params.wallet_exposure_limit * (1.0 + bot_params.entry_trailing_grid_ratio) + 1%
            if wallet_exposure == 0.0 {
                calc_grid_entry_short(&exchange_params, &state_params, &bot_params, &position)
            } else {
                let mut bot_params_modified = bot_params.clone();
                if wallet_exposure != 0.0 {
                    bot_params_modified.wallet_exposure_limit = bot_params.wallet_exposure_limit
                        * (1.0 + bot_params.entry_trailing_grid_ratio)
                        * 1.01;
                }
                calc_grid_entry_short(
                    &exchange_params,
                    &state_params,
                    &bot_params_modified,
                    &position,
                )
            }
        } else {
            calc_trailing_entry_short(
                &exchange_params,
                &state_params,
                &bot_params,
                &position,
                &trailing_price_bundle,
            )
        }
    }
}

pub fn calc_entries_long(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
    divergence_bundle: &DivergenceBundle,
) -> Vec<Order> {
    let mut entries = Vec::<Order>::new();
    let mut psize = position.size;
    let mut pprice = position.price;
    let mut bid = state_params.order_book.bid;
    for _ in 0..500 {
        let position_mod = Position {
            size: psize,
            price: pprice,
        };
        let mut state_params_mod = state_params.clone();
        state_params_mod.order_book.bid = bid;
        let entry = calc_next_entry_long(
            exchange_params,
            &state_params_mod,
            bot_params,
            &position_mod,
            &trailing_price_bundle,
            &divergence_bundle,
        );
        if entry.qty == 0.0 {
            break;
        }
        if !entries.is_empty() {
            if entry.order_type == OrderType::EntryTrailingNormalLong
                || entry.order_type == OrderType::EntryTrailingCroppedLong
            {
                break;
            }
            if entries[entries.len() - 1].price == entry.price {
                break;
            }
        }
        (psize, pprice) = calc_new_psize_pprice(
            psize,
            pprice,
            entry.qty,
            entry.price,
            exchange_params.qty_step,
        );
        bid = bid.min(entry.price);
        entries.push(entry);
    }
    entries
}

pub fn calc_entries_short(
    exchange_params: &ExchangeParams,
    state_params: &StateParams,
    bot_params: &BotParams,
    position: &Position,
    trailing_price_bundle: &TrailingPriceBundle,
) -> Vec<Order> {
    let mut entries = Vec::<Order>::new();
    let mut psize = position.size;
    let mut pprice = position.price;
    let mut ask = state_params.order_book.ask;
    for _ in 0..500 {
        let position_mod = Position {
            size: psize,
            price: pprice,
        };
        let mut state_params_mod = state_params.clone();
        state_params_mod.order_book.ask = ask;
        let entry = calc_next_entry_short(
            exchange_params,
            &state_params_mod,
            bot_params,
            &position_mod,
            &trailing_price_bundle,
        );
        if entry.qty == 0.0 {
            break;
        }
        if !entries.is_empty() {
            if entry.order_type == OrderType::EntryTrailingNormalShort
                || entry.order_type == OrderType::EntryTrailingCroppedShort
            {
                break;
            }
            if entries[entries.len() - 1].price == entry.price {
                break;
            }
        }
        (psize, pprice) = calc_new_psize_pprice(
            psize,
            pprice,
            entry.qty,
            entry.price,
            exchange_params.qty_step,
        );
        ask = ask.max(entry.price);
        entries.push(entry);
    }
    entries
}
