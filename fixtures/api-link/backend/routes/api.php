<?php

use App\Http\Controllers\CartController;
use App\Http\Controllers\MallController;
use App\Http\Controllers\NearbyMallController;
use Illuminate\Support\Facades\Route;

Route::prefix('v1')->group(function () {
    Route::get('malls', [MallController::class, 'index'])->name('malls.index');
    Route::get('malls/{mall:slug}', [MallController::class, 'show'])->name('malls.show');
    Route::get('malls/{mall:slug}/offers', [MallController::class, 'offers']);
    Route::get('nearby-malls', [NearbyMallController::class, 'index']);
    Route::get('carts', [CartController::class, 'index']);
    Route::post('carts/{merchant}/items', [CartController::class, 'add']);
    Route::patch('carts/{merchant}/items/{item}', [CartController::class, 'update']);
    Route::delete('carts/{merchant}/items/{item}', [CartController::class, 'remove']);
});
