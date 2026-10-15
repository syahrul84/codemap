<?php

use Illuminate\Support\Facades\Route;

Route::get('/', function () {
    return view('welcome');
});

Route::get('orders', 'OrderController@index')->name('orders.index');
Route::post('orders', 'OrderController@store')->name('orders.store');

Route::middleware('auth')->group(function () {
    Route::get('orders/{order}', 'OrderController@show')->name('orders.show');
    Route::get('reports', 'Admin\ReportController')->name('reports');
});
