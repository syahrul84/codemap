<?php

namespace App\Http\Controllers;

class ReportController extends Controller
{
    public function sales()
    {
        return ['data' => []];
    }

    public function sale(int $report)
    {
        return ['data' => ['id' => $report]];
    }

    public function stock()
    {
        return ['data' => []];
    }
}
