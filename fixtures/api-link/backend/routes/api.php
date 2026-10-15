<?php

use App\Http\Controllers\CartController;
use App\Http\Controllers\MallController;
use Illuminate\Support\Facades\Route;

Route::prefix('v1')->group(function () {
    Route::get('malls', [MallController::class, 'index'])->name('malls.index');
    Route::get('malls/{mall:slug}', [MallController::class, 'show'])->name('malls.show');
    Route::get('carts', [CartController::class, 'index']);
    Route::post('carts/{merchant}/items', [CartController::class, 'add']);
});
