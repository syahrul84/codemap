<?php

use App\Http\Controllers\OrderController;
use Illuminate\Support\Facades\Route;

Route::controller(OrderController::class)
    ->prefix('orders')
    ->name('orders.')
    ->group(function () {
        Route::get('/', 'index')->name('index');
        Route::get('/{order}', 'show')->name('show')->whereNumber('order');
    });

Route::view('/about', 'about')->name('about');
Route::redirect('/home', '/');
