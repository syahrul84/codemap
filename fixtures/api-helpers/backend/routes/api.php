<?php

use App\Http\Controllers\AssetController;
use Illuminate\Support\Facades\Route;

Route::get('assets', [AssetController::class, 'index']);
Route::post('assets', [AssetController::class, 'store']);
Route::get('assets/{asset}', [AssetController::class, 'show']);
Route::patch('assets/{asset}', [AssetController::class, 'update']);
Route::get('assets/{asset}/history', [AssetController::class, 'history']);
Route::get('lookups/categories', [AssetController::class, 'categories']);
