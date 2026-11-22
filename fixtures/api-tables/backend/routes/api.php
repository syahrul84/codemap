<?php

use App\Http\Controllers\PermitController;
use App\Http\Controllers\ReportController;
use Illuminate\Support\Facades\Route;

Route::get('permits/moving', [PermitController::class, 'index']);
Route::post('permits/moving', [PermitController::class, 'store']);
Route::get('permits/moving/{permit}', [PermitController::class, 'show']);
Route::delete('permits/moving/{permit}', [PermitController::class, 'destroy']);

Route::get('permits/delivery', [PermitController::class, 'index']);
Route::post('permits/delivery', [PermitController::class, 'store']);
Route::get('permits/delivery/{permit}', [PermitController::class, 'show']);

Route::get('permits/renovation', [PermitController::class, 'index']);
Route::post('permits/renovation', [PermitController::class, 'store']);
Route::get('permits/renovation/{permit}', [PermitController::class, 'show']);
Route::delete('permits/renovation/{permit}', [PermitController::class, 'destroy']);
Route::get('permits/renovation/{permit}/extension', [PermitController::class, 'extensions']);
Route::get('permits/renovation/{permit}/extension/{extension}', [PermitController::class, 'extension']);

Route::get('setup/approvers', [PermitController::class, 'approvers']);

Route::get('visitors', [PermitController::class, 'index']);
Route::post('visitors', [PermitController::class, 'store']);
Route::get('visitors/{visitor}', [PermitController::class, 'show']);
Route::patch('visitors/{visitor}', [PermitController::class, 'update']);

Route::get('reports/sales', [ReportController::class, 'sales']);
Route::get('reports/sales/{report}', [ReportController::class, 'sale']);
Route::get('reports/stock', [ReportController::class, 'stock']);
