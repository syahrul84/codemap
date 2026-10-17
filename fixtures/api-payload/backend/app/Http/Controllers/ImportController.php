<?php

namespace App\Http\Controllers;

use Illuminate\Http\Request;

class ImportController extends Controller
{
    public function store(Request $request)
    {
        $result = app('importer')->run($request);

        return response()->json(['data' => $result]);
    }
}
