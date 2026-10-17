<?php

namespace App\Http\Controllers;

class InviteController extends Controller
{
    public function resend(int $invite)
    {
        return response()->json(['data' => $invite]);
    }

    public function void(int $invite)
    {
        return response()->json(['data' => $invite]);
    }

    public function quote(int $quote)
    {
        return response()->json(['data' => $quote]);
    }
}
