<?php
use Illuminate\Support\Facades\Route;

$shared = function (): void {
    Route::get('/widgets/{id}', fn () => 'ok')->name('widgets.show');
};

Route::prefix('{lang}')->group($shared);
Route::group([], $shared);
