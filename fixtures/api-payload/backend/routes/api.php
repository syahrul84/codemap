<?php

use App\Http\Controllers\ImportController;
use App\Http\Controllers\InviteController;
use App\Http\Controllers\NoteController;
use App\Http\Controllers\OfferController;
use Illuminate\Support\Facades\Route;

Route::prefix('v1')->group(function () {
    Route::post('offers', [OfferController::class, 'store']);
    Route::patch('offers/{offer}', [OfferController::class, 'update']);
    Route::post('notes', [NoteController::class, 'store']);
    Route::get('notes', [NoteController::class, 'index']);
    Route::post('imports', [ImportController::class, 'store']);
    Route::put('invites/{invite}/resend', [InviteController::class, 'resend']);
    Route::put('invites/{invite}/void', [InviteController::class, 'void']);
    Route::patch('quotes/{quote}', [InviteController::class, 'quote']);
    Route::put('widgets/{widget}/archive', [InviteController::class, 'quote']);
    Route::put('widgets/{widget}/retire', [InviteController::class, 'quote']);
});
