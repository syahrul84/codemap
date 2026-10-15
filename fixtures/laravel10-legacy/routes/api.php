<?php

use App\Http\Controllers\OrderController;
use Illuminate\Support\Facades\Route;

Route::apiResource('orders', OrderController::class);
Route::post('orders/{order}/pay', [OrderController::class, 'markPaid'])->name('orders.pay');
