<?php

use Illuminate\Support\Facades\Route;

Route::get('ping', fn () => ['ok' => true]);
