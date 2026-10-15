<?php

namespace App\Http\Controllers\Admin;

use App\Http\Controllers\Controller;

class ReportController extends Controller
{
    /**
     * Monthly sales report.
     */
    public function __invoke()
    {
        return view('reports.index');
    }
}
