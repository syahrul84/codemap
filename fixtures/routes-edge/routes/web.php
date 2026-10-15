<?php

use App\Http\Controllers\Admin;
use App\Http\Controllers\CommentController;
use App\Http\Controllers\PhotoController;
use App\Http\Controllers\ProfileController;
use Illuminate\Support\Facades\Route;

Route::match(['get', 'post'], '/search', [PhotoController::class, 'search'])->name('search');
Route::any('/ping', fn () => 'pong');
Route::permanentRedirect('/old', '/new');

Route::group(['prefix' => 'admin', 'as' => 'admin.', 'middleware' => ['auth', 'can:admin']], function () {
    Route::get('/dashboard', Admin\DashboardController::class)->name('dashboard');
    require __DIR__.'/admin.php';
});

Route::domain('{account}.example.com')->group(function () {
    Route::get('/', [PhotoController::class, 'index'])->name('account.home');
});

Route::resources([
    'photos' => PhotoController::class,
    'videos' => \App\Http\Controllers\VideoController::class,
]);

Route::resource('photos.comments', CommentController::class)->shallow()->only(['index', 'show']);
Route::apiResource('user-profiles', ProfileController::class)
    ->parameters(['user-profiles' => 'profile'])
    ->names(['index' => 'profiles.list'])
    ->only(['index', 'show']);
Route::singleton('settings', ProfileController::class);

Route::get('/legacy', 'LegacyController@index');

foreach (['a', 'b'] as $letter) {
    Route::get('/letters/'.$letter, fn () => $letter);
}

$uri = '/dynamic';
Route::get($uri, fn () => 'x');

Route::get('/macro', fn () => 'm')->customMacro();

if (app()->isLocal()) {
    Route::get('/debug', fn () => 'debug');
}

Route::get('/no-session', fn () => 'x')->withoutMiddleware('web');

Route::fallback(fn () => abort(404));
